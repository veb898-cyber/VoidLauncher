//! User-facing diagnostics (roadmap stage 6): system inventory, automatic
//! common-problem detection for a selected instance, and the crash analyzer.
//!
//! `run_diagnostics` is read-only: it inspects the OS/CPU/RAM/GPU/disk/Java,
//! probes reachability of the download services, and — when an instance is
//! selected — reuses the offline verify scan plus instance settings to flag
//! the usual launch blockers (missing loader, Java mismatch, over-allocated
//! memory, broken launcher files, invalid content archives).
//!
//! `analyze_crash` analyzes one specific run (identified by its launcher
//! session log): reads the crash report written inside that run's time
//! window (or the tail of the run's own log when there is none) and
//! classifies the failure into a stable kind with a confidence verdict, so
//! the UI can show a likely cause instead of a raw log. A clean exit with
//! no artifacts reports `source: "clean"` instead of a false "crash".

use crate::config::AppConfig;
use crate::instances::{self, LoaderType};
use crate::verify::{self, VerifyReport};
use serde::Serialize;
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

// ══════════════════════════ report types ══════════════════════════

#[derive(Debug, Serialize, Clone)]
pub struct DiagCheck {
    /// Stable id → i18n label in the frontend (`diag.check_<id>`).
    pub id: String,
    /// `"ok"` | `"warn"` | `"error"` | `"info"`.
    pub status: String,
    /// Pre-formatted data value (numbers, versions, names). May be empty.
    pub value: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct DiagIssue {
    /// Stable id → i18n message (`diag.issue_<id>`) and hint (`diag.fix_<id>`).
    pub id: String,
    /// `"warn"` | `"error"`.
    pub severity: String,
    /// Substitution values for the i18n message.
    pub params: HashMap<String, String>,
}

#[derive(Debug, Serialize, Clone)]
pub struct DiagInstanceInfo {
    pub name: String,
    pub mc_version: String,
    pub loader: String,
    pub loader_version: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
pub struct DiagReport {
    pub launcher_version: String,
    pub checks: Vec<DiagCheck>,
    pub issues: Vec<DiagIssue>,
    pub instance: Option<DiagInstanceInfo>,
    /// Full verify report for the selected instance (categories + counts),
    /// rendered by the UI as the "instance files" group.
    pub verify: Option<VerifyReport>,
}

fn params(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

// ══════════════════════════ system inventory ══════════════════════════

fn detect_gpus() -> Vec<String> {
    // GPU names + driver versions live under the display class key. Each
    // numbered subkey is one adapter; DriverDesc is the friendly name.
    let mut names: Vec<String> = Vec::new();
    let hkml = winreg::RegKey::predef(winreg::enums::HKEY_LOCAL_MACHINE);
    let class = match hkml.open_subkey(
        r"SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}",
    ) {
        Ok(k) => k,
        Err(_) => return names,
    };
    for sub in class.enum_keys().flatten() {
        if let Ok(adapter) = class.open_subkey(&sub) {
            let desc: String = adapter.get_value("DriverDesc").unwrap_or_default();
            if desc.trim().is_empty() {
                continue;
            }
            let driver: String = adapter.get_value("DriverVersion").unwrap_or_default();
            let entry = if driver.trim().is_empty() {
                desc
            } else {
                format!("{} ({})", desc, driver)
            };
            if !names.contains(&entry) {
                names.push(entry);
            }
        }
    }
    names
}

/// Total/free space (GiB) of the volume that holds `data_dir`.
fn disk_for_dir(dir: &Path) -> Option<(u64, u64)> {
    use sysinfo::Disks;
    let disks = Disks::new_with_refreshed_list();
    let dir_str = dir.to_string_lossy().to_lowercase();
    let disk = disks
        .list()
        .iter()
        .find(|d| {
            let name = d.name().to_string_lossy().to_lowercase();
            !name.is_empty() && dir_str.starts_with(&name)
        })
        .or_else(|| disks.list().first())?;
    Some((disk.total_space(), disk.available_space()))
}

fn gb(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
}

fn mb(bytes: u64) -> u64 {
    bytes / (1024 * 1024)
}

struct SystemInfo {
    checks: Vec<DiagCheck>,
    issues: Vec<DiagIssue>,
}

fn collect_system(config: &AppConfig) -> SystemInfo {
    use sysinfo::System;

    let mut checks = Vec::new();
    let mut issues = Vec::new();

    let mut sys = System::new();
    sys.refresh_memory();
    sys.refresh_cpu_all();

    let os = System::name().unwrap_or_else(|| "Unknown".to_string());
    let os_ver = System::os_version()
        .or_else(System::kernel_version)
        .unwrap_or_default();
    checks.push(DiagCheck {
        id: "os".into(),
        status: "ok".into(),
        value: format!("{} {}", os, os_ver).trim().to_string(),
    });

    let cpu = sys.cpus().first().map(|c| c.brand().trim().to_string());
    let physical = sys.physical_core_count().unwrap_or(0);
    let logical = sys.cpus().len();
    checks.push(DiagCheck {
        id: "cpu".into(),
        status: if cpu.is_some() { "ok" } else { "warn" }.into(),
        value: match cpu {
            Some(brand) if physical > 0 => format!("{} · {} / {}", brand, physical, logical),
            Some(brand) => format!("{} · {}", brand, logical),
            None => String::new(),
        },
    });

    let total_ram = sys.total_memory();
    let avail_ram = sys.available_memory();
    checks.push(DiagCheck {
        id: "ram".into(),
        status: "ok".into(),
        value: format!("{} / {} GB", mb(avail_ram), mb(total_ram)),
    });

    let gpus = detect_gpus();
    checks.push(DiagCheck {
        id: "gpu".into(),
        status: if gpus.is_empty() { "info" } else { "ok" }.into(),
        value: if gpus.is_empty() {
            String::new()
        } else {
            gpus.join(", ")
        },
    });

    let disk = disk_for_dir(&config.data_dir);
    checks.push(DiagCheck {
        id: "disk".into(),
        status: match disk {
            Some((_, free)) if free < 1024 * 1024 * 1024 => "error",
            Some((_, free)) if free < 3 * 1024 * 1024 * 1024 => "warn",
            _ => "ok",
        }
        .into(),
        value: match disk {
            Some((total, free)) => format!("{} / {} GB", gb(free), gb(total)),
            None => String::new(),
        },
    });
    if let Some((_, free)) = disk {
        if free < 1024 * 1024 * 1024 {
            issues.push(DiagIssue {
                id: "disk_free".into(),
                severity: "error".into(),
                params: params(&[("free", &gb(free))]),
            });
        } else if free < 3 * 1024 * 1024 * 1024 {
            issues.push(DiagIssue {
                id: "disk_free".into(),
                severity: "warn".into(),
                params: params(&[("free", &gb(free))]),
            });
        }
    }

    let mut majors: Vec<u32> = crate::launch::find_all_java_installations(&config.data_dir)
        .into_iter()
        .map(|j| j.major_version)
        .collect();
    majors.sort_unstable();
    majors.dedup();
    checks.push(DiagCheck {
        id: "java".into(),
        status: if majors.is_empty() { "warn" } else { "ok" }.into(),
        value: majors
            .iter()
            .map(|m| m.to_string())
            .collect::<Vec<_>>()
            .join(", "),
    });

    // Default memory allocation vs. physical RAM.
    let alloc = config.default_memory_mb as u64;
    if alloc > mb(total_ram) {
        issues.push(DiagIssue {
            id: "memory".into(),
            severity: "error".into(),
            params: params(&[
                ("alloc", &alloc.to_string()),
                ("total", &mb(total_ram).to_string()),
            ]),
        });
    }

    checks.push(DiagCheck {
        id: "launcher".into(),
        status: "ok".into(),
        value: env!("CARGO_PKG_VERSION").to_string(),
    });

    SystemInfo { checks, issues }
}

// ══════════════════════════ network reachability ══════════════════════════

const NETWORK_HOSTS: &[(&str, &str)] = &[
    (
        "mojang",
        "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json",
    ),
    ("modrinth", "https://api.modrinth.com/v2/search?limit=1"),
    ("curseforge", "https://api.curseforge.com/"),
];

/// Probe the download services. Any HTTP response (even 4xx) means the
/// service is reachable; only transport errors/timeouts count as down.
async fn check_network() -> (usize, Vec<String>) {
    let Ok(client) = crate::download::global_http_client() else {
        return (
            0,
            NETWORK_HOSTS.iter().map(|(n, _)| n.to_string()).collect(),
        );
    };
    let probes = NETWORK_HOSTS.iter().map(|(name, url)| {
        let client = client.clone();
        let url = (*url).to_string();
        let name = (*name).to_string();
        async move {
            let ok = client
                .get(&url)
                .timeout(Duration::from_secs(5))
                .send()
                .await
                .is_ok();
            (name, ok)
        }
    });
    let results = futures::future::join_all(probes).await;
    let total = results.len();
    let failed: Vec<String> = results
        .into_iter()
        .filter(|(_, ok)| !ok)
        .map(|(name, _)| name)
        .collect();
    (total - failed.len(), failed)
}

// ══════════════════════════ instance diagnostics ══════════════════════════

/// Offline instance checks: verify scan + settings problems. Returns the
/// instance info, its issues, the verify report, and the derived "files"
/// check status.
fn collect_instance(
    config: &AppConfig,
    instance_name: &str,
) -> Result<(DiagInstanceInfo, Vec<DiagIssue>, Option<VerifyReport>), String> {
    let instance = instances::get_instance(&config.instances_dir(), instance_name)
        .map_err(|e| e.to_string())?;

    let mut issues = Vec::new();

    // Loader present?
    if instance.loader != LoaderType::Vanilla && instance.loader_profile.is_none() {
        issues.push(DiagIssue {
            id: "loader_missing".into(),
            severity: "error".into(),
            params: params(&[("loader", crate::packlock::loader_slug(&instance.loader))]),
        });
    }

    // Java: the version manifest says which major the game needs.
    if let Some(version_info) =
        verify::load_local_version_info(&config.versions_dir(), &instance.mc_version)
    {
        let required = version_info.required_java_major();
        let custom = instance
            .java_path
            .clone()
            .or_else(|| config.java_path.clone());
        match custom {
            Some(path) => match crate::java::probe_java_by_path(&path) {
                Some(install) => {
                    if install.major_version != required {
                        issues.push(DiagIssue {
                            id: "java_mismatch".into(),
                            severity: "error".into(),
                            params: params(&[
                                ("required", &required.to_string()),
                                ("actual", &install.major_version.to_string()),
                            ]),
                        });
                    }
                }
                None => {
                    issues.push(DiagIssue {
                        id: "java_unusable".into(),
                        severity: "error".into(),
                        params: params(&[("path", &path.to_string_lossy())]),
                    });
                }
            },
            None => {
                let has = crate::launch::find_all_java_installations(&config.data_dir)
                    .iter()
                    .any(|j| j.major_version == required);
                if !has {
                    issues.push(DiagIssue {
                        id: "java_missing".into(),
                        severity: "warn".into(),
                        params: params(&[("required", &required.to_string())]),
                    });
                }
            }
        }
    }

    // Instance memory vs. physical RAM.
    let total_mb = crate::config::detect_total_ram_mb();
    let alloc_mb = instance.memory_mb.unwrap_or(config.default_memory_mb) as u64;
    if total_mb > 0 && alloc_mb > total_mb {
        issues.push(DiagIssue {
            id: "memory".into(),
            severity: "error".into(),
            params: params(&[
                ("alloc", &alloc_mb.to_string()),
                ("total", &total_mb.to_string()),
            ]),
        });
    }

    // Full offline integrity scan (launcher files + content archives).
    let report = verify::verify_instance_scoped(config, &instance, verify::VerifyScope::All);
    if report.repairable_count > 0 {
        issues.push(DiagIssue {
            id: "files".into(),
            severity: "error".into(),
            params: params(&[("n", &report.repairable_count.to_string())]),
        });
    }
    if report.content_issues > 0 {
        issues.push(DiagIssue {
            id: "content".into(),
            severity: "warn".into(),
            params: params(&[("n", &report.content_issues.to_string())]),
        });
    }

    let info = DiagInstanceInfo {
        name: instance.name.clone(),
        mc_version: instance.mc_version.clone(),
        loader: crate::packlock::loader_slug(&instance.loader).to_string(),
        loader_version: instance.loader_version.clone(),
    };
    Ok((info, issues, Some(report)))
}

// ══════════════════════════ entry point ══════════════════════════

/// Gather the full diagnostics report. Instance checks run on a worker
/// thread (the verify scan walks thousands of files); the network probes
/// run concurrently with a 5 s per-host timeout.
pub async fn run_diagnostics(config: AppConfig, instance_name: Option<String>) -> DiagReport {
    let (sys, inst, network) = {
        let sys_config = config.clone();
        let inst_config = config.clone();
        let inst_name = instance_name.clone();
        let sys_task = tauri::async_runtime::spawn_blocking(move || collect_system(&sys_config));
        let inst_task = tauri::async_runtime::spawn_blocking(move || match inst_name {
            Some(name) => collect_instance(&inst_config, &name).ok(),
            None => None,
        });
        let net_task = check_network();
        let (sys_res, inst_res, net_res) =
            futures::future::join3(sys_task, inst_task, net_task).await;
        (
            sys_res.unwrap_or_else(|_| SystemInfo {
                checks: Vec::new(),
                issues: Vec::new(),
            }),
            inst_res.ok().flatten(),
            net_res,
        )
    };

    let mut checks = sys.checks;
    let mut issues = sys.issues;
    let (instance, inst_issues, verify_report) = match inst {
        Some((info, inst_issues, report)) => (Some(info), inst_issues, report),
        None => (None, Vec::new(), None),
    };

    // Network row + issue (inserted before the instance group).
    let (net_ok, net_failed) = network;
    let net_total = NETWORK_HOSTS.len();
    let net_status = if net_failed.is_empty() {
        "ok"
    } else if net_ok == 0 {
        "error"
    } else {
        "warn"
    };
    // Place the network check right after the launcher row.
    if let Some(pos) = checks.iter().position(|c| c.id == "launcher") {
        checks.insert(
            pos,
            DiagCheck {
                id: "network".into(),
                status: net_status.into(),
                value: format!("{}/{}", net_ok, net_total),
            },
        );
    }
    if !net_failed.is_empty() {
        issues.push(DiagIssue {
            id: "network".into(),
            severity: net_status.into(),
            params: params(&[("failed", &net_failed.join(", "))]),
        });
    }

    // Instance group: identity row + files row, then its issues.
    if let Some(ref info) = instance {
        let files_status = match &verify_report {
            Some(r) if r.repairable_count > 0 => "error",
            Some(r) if r.content_issues > 0 => "warn",
            Some(_) => "ok",
            None => "info",
        };
        let files_value = verify_report
            .as_ref()
            .map(|r| {
                let launcher_issues: u32 = r.categories.iter().map(|c| c.issue_count).sum();
                (launcher_issues + r.content_issues).to_string()
            })
            .unwrap_or_default();
        checks.push(DiagCheck {
            id: "instance".into(),
            status: "ok".into(),
            value: format!(
                "{} / {}{}",
                info.mc_version,
                info.loader,
                info.loader_version
                    .as_ref()
                    .map(|v| format!(" {}", v))
                    .unwrap_or_default()
            ),
        });
        checks.push(DiagCheck {
            id: "files".into(),
            status: files_status.into(),
            value: files_value,
        });
        issues.extend(inst_issues);
    }

    DiagReport {
        launcher_version: env!("CARGO_PKG_VERSION").to_string(),
        checks,
        issues,
        instance,
        verify: verify_report,
    }
}

// ══════════════════════════ crash analyzer ══════════════════════════

#[derive(Debug, Serialize, Clone)]
pub struct CrashAnalysis {
    /// `"likely"` | `"possible"` | `"unknown"`.
    pub verdict: String,
    /// Stable kind → i18n (`diag.crash_<kind>`, `diag.crash_hint_<kind>`).
    pub kind: String,
    /// Mod/file suspected to cause the crash, when extractable.
    pub culprit: Option<String>,
    /// Evidence: the first log line matching the classification.
    pub detail: String,
    /// `"crash-report"` | `"session-log"` | `"latest-log"` | `"clean"` | `"none"`.
    pub source: String,
    /// Absolute path of the crash report, when one exists (for opening it).
    pub report_path: Option<String>,
}

struct Rule {
    kind: &'static str,
    verdict: &'static str,
    needles: &'static [&'static str],
}

/// Ordered rules: the first match wins. Needles are matched
/// case-insensitively (crash files mix casing freely).
const RULES: &[Rule] = &[
    Rule {
        kind: "oom",
        verdict: "likely",
        needles: &["outofmemoryerror", "java heap space", "out of memory"],
    },
    Rule {
        kind: "java_version",
        verdict: "likely",
        needles: &[
            "unsupportedclassversionerror",
            "compiled by a more recent version of the java runtime",
        ],
    },
    Rule {
        kind: "jvm_args",
        verdict: "likely",
        needles: &[
            "unrecognized vm option",
            "could not create the java virtual machine",
            "error occurred during initialization of vm",
        ],
    },
    Rule {
        kind: "launch_config",
        verdict: "likely",
        needles: &["could not find or load main class"],
    },
    Rule {
        kind: "mod_crash",
        verdict: "likely",
        needles: &[
            "mixinapplyerror",
            "mixin apply failed",
            "invalidinjectionexception",
            "failed loading mod",
        ],
    },
    // Fabric's mod-resolution failure ("Incompatible mods found!" is the
    // modern wording; the exception class appears in the same block). Runs
    // before the dependency rule so the loader-version refinement in
    // `classify_run_crash` can sharpen it to `loader_outdated` when the log
    // blames the loader version rather than a missing mod.
    Rule {
        kind: "mod_dependency",
        verdict: "likely",
        needles: &["incompatible mods found", "formattedexception"],
    },
    Rule {
        kind: "missing_dependency",
        verdict: "likely",
        needles: &[
            "incompatible mod set",
            "but it's missing",
            "but it's not installed",
            "missing mods:",
        ],
    },
    Rule {
        kind: "missing_class",
        verdict: "possible",
        needles: &["noclassdeffounderror", "classnotfoundexception"],
    },
    Rule {
        kind: "mod_conflict",
        verdict: "possible",
        needles: &[
            "nosuchmethoderror",
            "nosuchfielderror",
            "abstractmethoderror",
        ],
    },
    Rule {
        kind: "graphics",
        verdict: "possible",
        needles: &["glfw error", "failed to initialize opengl", "lwjglerror"],
    },
];

/// ASCII-lowercase preserves byte indices (non-ASCII bytes pass through
/// unchanged), so positions found in `low` map 1:1 back into `original`.
fn find_ci(low: &str, needle: &str) -> Option<usize> {
    low.find(needle)
}

/// A log-style WARN line ("[12:00:00] [main/WARN]: …"). Forge/NeoForge emit
/// benign "Error loading class: … ClassNotFoundException" warnings while
/// probing optional compat hooks; they are not crash evidence.
fn is_warn_line(line: &str) -> bool {
    let low = line.to_ascii_lowercase();
    low.contains("/warn]") || low.contains("[warn")
}

/// First line containing any needle, returned with its byte offset in
/// `original`. Strong (non-WARN) lines beat WARN ones regardless of
/// position; with `reject_warn` a WARN-only match yields `None` (used by
/// the noise-prone rules).
fn evidence_line(original: &str, needles: &[&str], reject_warn: bool) -> Option<(String, usize)> {
    let mut weak: Option<(String, usize)> = None;
    let mut offset = 0usize;
    for line in original.split('\n') {
        let line_start = offset;
        offset += line.len() + 1;
        let line_low = line.to_ascii_lowercase();
        if needles.iter().any(|n| line_low.contains(n)) {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let value: String = trimmed.chars().take(400).collect();
            if is_warn_line(trimmed) {
                if weak.is_none() {
                    weak = Some((value, line_start));
                }
                continue;
            }
            return Some((value, line_start));
        }
    }
    if reject_warn {
        None
    } else {
        weak
    }
}

/// Pull a mod/file name out of the evidence text. Strategies are ordered
/// from most to least specific; all are plain substring scans. `focus`,
/// when given, is the byte offset (within `original`) of the evidence line:
/// the jar-path scan then prefers the line closest to it instead of the
/// first one, so a stack frame beats an unrelated "mods/…jar" line.
fn extract_culprit(original: &str, focus: Option<usize>) -> Option<String> {
    let low = original.to_ascii_lowercase();

    // "… from mod [sodium] …" / "… from mod 'sodium' …" (Mixin failures).
    // ASCII-lowering keeps indices aligned, so slice the ORIGINAL text.
    if let Some(pos) = find_ci(&low, "from mod ") {
        let start = pos + "from mod ".len();
        let rest = &original[start..];
        let rest_low = &low[start..];
        for (open, close) in [('[', ']'), ('\'', '\''), ('"', '"')] {
            if rest_low.starts_with(open) {
                let inner = &rest[open.len_utf8()..];
                let inner_low = &rest_low[open.len_utf8()..];
                if let Some(end) = inner_low.find(close) {
                    let name = inner[..end].trim();
                    if !name.is_empty() {
                        return Some(name.chars().take(80).collect());
                    }
                }
            }
        }
        // Bare token: "from mod sodium failed…"
        let end = rest
            .find(|c: char| c.is_whitespace() || c == ',' || c == ')')
            .unwrap_or(rest.len());
        let name = rest[..end].trim_matches(|c: char| c == '\'' || c == '"');
        if !name.is_empty() {
            return Some(name.chars().take(80).collect());
        }
    }

    // "Failed loading mod file 'create-1.20.1.jar'" (FML/NeoForge):
    // prefer the quoted name near the phrase, else the next token.
    if let Some(pos) = find_ci(&low, "failed loading mod") {
        let start = pos + "failed loading mod".len();
        let rest = &original[start..];
        let rest_low = &low[start..];
        for q in ['\'', '"'] {
            if let Some(qpos) = rest_low.find(q).filter(|p| *p < 160) {
                let inner_start = qpos + 1;
                if let Some(end) = rest_low[inner_start..].find(q) {
                    let name = rest[inner_start..inner_start + end].trim();
                    if !name.is_empty() {
                        return Some(name.chars().take(80).collect());
                    }
                }
            }
        }
        let mut cursor = 0;
        loop {
            let tail = rest[cursor..].trim_start();
            cursor = rest.len() - tail.len();
            if tail.is_empty() {
                break;
            }
            let end = tail
                .find(|c: char| c.is_whitespace() || c == ',' || c == ')' || c == '\'' || c == '"')
                .unwrap_or(tail.len());
            if end == 0 {
                // The tail begins with a delimiter (e.g. ",)" right after the
                // phrase) — skip that one char and keep scanning, otherwise
                // `cursor` would never advance and this loop would spin.
                cursor += tail.chars().next().map(char::len_utf8).unwrap_or(1);
                continue;
            }
            let word = &tail[..end];
            cursor += end;
            if word.eq_ignore_ascii_case("file") || word.eq_ignore_ascii_case("mod") {
                continue;
            }
            return Some(word.chars().take(80).collect());
        }
    }

    // A jar path in the text: "… (mods/sodium.jar)". With a focus point the
    // line nearest to the evidence wins; without one the first match wins
    // (for callers that scan isolated snippets).
    let mut best: Option<(usize, String)> = None;
    let mut offset = 0usize;
    for line in original.split('\n') {
        let line_start = offset;
        offset += line.len() + 1;
        let line_low = line.to_ascii_lowercase();
        if line_low.contains(".jar") && (line_low.contains("mods/") || line_low.contains("mods\\"))
        {
            let Some(start) = line_low.rfind(['/', '\\']).map(|p| p + 1) else {
                continue;
            };
            let Some(rel) = line_low[start..].find(".jar") else {
                continue;
            };
            let end = start + rel + 4;
            let name = line[start..end].trim();
            if !name.is_empty() {
                let dist = focus.map(|f| line_start.abs_diff(f)).unwrap_or(0);
                if best.as_ref().map(|(d, _)| dist < *d).unwrap_or(true) {
                    best = Some((dist, name.chars().take(120).collect()));
                }
            }
        }
    }
    best.map(|(_, name)| name)
}

/// Context window (bytes) taken around the evidence line for culprit
/// extraction: wide enough for a stack trace, narrow enough that unrelated
/// lines from the rest of the log can't be blamed.
const EVIDENCE_CTX_BEFORE: usize = 2000;
const EVIDENCE_CTX_AFTER: usize = 4000;

fn clamp_to_char_boundary(text: &str, mut pos: usize) -> usize {
    while pos > 0 && !text.is_char_boundary(pos) {
        pos -= 1;
    }
    pos
}

/// Classify a text blob (crash report or log tail). Returns the matched
/// rule, the evidence line, and the culprit extracted around that line.
fn classify_crash(text: &str) -> Option<(&'static Rule, String, Option<String>)> {
    for rule in RULES {
        // These needles are routinely hit by benign WARN probes (Forge
        // "Error loading class" for optional compat hooks), so a WARN-only
        // match is not evidence of a crash.
        let reject_warn = matches!(rule.kind, "missing_class" | "mod_conflict");
        let Some((detail, line_start)) = evidence_line(text, rule.needles, reject_warn) else {
            continue;
        };
        let from = clamp_to_char_boundary(text, line_start.saturating_sub(EVIDENCE_CTX_BEFORE));
        let to = clamp_to_char_boundary(text, (line_start + EVIDENCE_CTX_AFTER).min(text.len()));
        let culprit = extract_culprit(&text[from..to], Some(line_start - from));
        return Some((rule, detail, culprit));
    }
    None
}

/// First `'…'` or `"…"` quoted span of a line (short, non-empty).
fn first_quoted(line: &str) -> Option<String> {
    for q in ['\'', '"'] {
        if let Some(inner) = line.split(q).nth(1) {
            let name = inner.trim();
            if !name.is_empty() {
                return Some(name.chars().take(80).collect());
            }
        }
    }
    None
}

/// Loader-version reading of a Fabric "Incompatible mods found!" failure:
/// when the log explains that a mod needs a *newer loader* («… requires
/// version 0.19.5 or later of mod 'Fabric Loader'», «Replace mod 'Fabric
/// Loader' … with version … or later», or the solver's «{depends
/// fabricloader @ [>=0.19.5]}» lines), the run died on the loader version —
/// updating the loader fixes it, removing mods would not.
struct LoaderOutdatedHint {
    /// The mod that raised the requirement, when the log names one.
    culprit: Option<String>,
    /// The most actionable log line (the "Replace … or later" sentence).
    detail: Option<String>,
}

fn fabric_loader_outdated(text: &str) -> Option<LoaderOutdatedHint> {
    let low = text.to_ascii_lowercase();
    if !(low.contains("fabricloader") || low.contains("fabric loader")) {
        return None;
    }
    let about_loader = low.contains("of mod 'fabric loader'")
        || low.contains("of mod \"fabric loader\"")
        || low.contains("replace mod 'fabric loader'")
        || low.contains("replace mod \"fabric loader\"")
        || low.contains("depends fabricloader");
    if !about_loader {
        return None;
    }
    let mut hint = LoaderOutdatedHint {
        culprit: None,
        detail: None,
    };
    for line in text.split('\n') {
        let line_low = line.to_ascii_lowercase();
        if hint.detail.is_none()
            && line_low.contains(" or later")
            && line_low.contains("fabric loader")
        {
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                hint.detail = Some(trimmed.chars().take(400).collect());
            }
        }
        if hint.culprit.is_none() && line_low.contains("requires version") {
            if let Some(name) =
                first_quoted(line).filter(|n| !n.to_ascii_lowercase().contains("fabric loader"))
            {
                hint.culprit = Some(name);
            }
        }
        if hint.detail.is_some() && hint.culprit.is_some() {
            break;
        }
    }
    Some(hint)
}

/// `classify_crash` + the run-level refinement: a generic "mods are
/// incompatible" hit is sharpened to `loader_outdated` when the log shows a
/// mod demanding a newer loader. Returns `(kind, verdict, detail, culprit)`.
fn classify_run_crash(text: &str) -> Option<(String, &'static str, String, Option<String>)> {
    let (rule, detail, culprit) = classify_crash(text)?;
    let mut kind = rule.kind.to_string();
    let mut detail = detail;
    let mut culprit = culprit;
    if rule.kind == "mod_dependency" {
        if let Some(hint) = fabric_loader_outdated(text) {
            kind = "loader_outdated".to_string();
            if let Some(d) = hint.detail {
                detail = d;
            }
            if culprit.is_none() {
                culprit = hint.culprit;
            }
        }
    }
    Some((kind, rule.verdict, detail, culprit))
}

const MAX_REPORT_BYTES: u64 = 4 * 1024 * 1024;
const MAX_LOG_TAIL_BYTES: u64 = 2 * 1024 * 1024;

/// Read a crash report (plain `.txt` or gzipped `.txt.gz`), capped.
fn read_report(path: &Path) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut buf: Vec<u8> = Vec::new();
    if path
        .extension()
        .map(|e| e.eq_ignore_ascii_case("gz"))
        .unwrap_or(false)
    {
        let decoder = flate2::read::GzDecoder::new(&mut file);
        let mut taken = decoder.take(MAX_REPORT_BYTES);
        let _ = std::io::copy(&mut taken, &mut buf);
    } else {
        let mut taken = file.take(MAX_REPORT_BYTES);
        taken.read_to_end(&mut buf).ok()?;
    }
    String::from_utf8(buf).ok()
}

/// Read the tail of a possibly huge log file.
fn read_tail(path: &Path, cap: u64) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(cap);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::new();
    file.take(cap).read_to_end(&mut buf).ok()?;
    String::from_utf8(buf).ok()
}

/// Newest crash report in the instance (by mtime), `.txt` or `.txt.gz`.
/// When `window` is given, only reports whose mtime falls inside it are
/// considered — the window bounds the analysis to one specific run, so an
/// old report from days ago can't be attributed to a fresh launch.
fn newest_crash_report(
    crash_dir: &Path,
    window: Option<(std::time::SystemTime, std::time::SystemTime)>,
) -> Option<PathBuf> {
    let entries = std::fs::read_dir(crash_dir).ok()?;
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_lowercase();
        if !(name.ends_with(".txt") || name.ends_with(".txt.gz")) {
            continue;
        }
        let mtime = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        if let Some((from, to)) = window {
            if mtime < from || mtime > to {
                continue;
            }
        }
        if best.as_ref().map(|(t, _)| mtime > *t).unwrap_or(true) {
            best = Some((mtime, path));
        }
    }
    best.map(|(_, p)| p)
}

/// The exit line the last pipe-reader appends to the session log:
/// "[INFO] [launch] Game exited with code N". Last occurrence wins (the
/// final line is chronologically last).
fn exit_code_from_session_log(text: &str) -> Option<i32> {
    const NEEDLE: &str = "Game exited with code ";
    let mut found = None;
    let mut rest = text;
    while let Some(pos) = rest.find(NEEDLE) {
        let after = &rest[pos + NEEDLE.len()..];
        let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(code) = digits.parse::<i32>() {
            found = Some(code);
        }
        rest = after;
    }
    found
}

/// Analyze a specific run of an instance. The run is identified by its
/// launcher session log (`{safe}_{YYYYMMDD}_{HHMMSS}.log`): the filename
/// carries the start time, the file mtime the end. Crash reports are only
/// considered inside that window — otherwise a normal close after an old
/// crash would surface a stale report as "cause unknown". A run whose log
/// records a clean exit (code 0) and has no report is reported as `clean`;
/// only failed runs get their unified log classified, with `latest.log` as
/// the last fallback (trustworthy only when it belongs to the same run).
///
/// `session_path: None` resolves to the newest session of the instance
/// (used by the auto-analysis right after exit, when the current-log path
/// is already cleared).
pub fn analyze_crash(
    config: &AppConfig,
    instance_name: &str,
    session_path: Option<&str>,
) -> Result<CrashAnalysis, String> {
    let instance = instances::get_instance(&config.instances_dir(), instance_name)
        .map_err(|e| e.to_string())?;
    let mc_dir = instance.minecraft_dir(&config.instances_dir());

    // Bound the analysis to one run when we can identify it.
    let owned = match session_path {
        Some(p) => PathBuf::from(p),
        None => crate::game_logs::newest_session_for_instance(&config.data_dir, instance_name)
            .unwrap_or_default(),
    };
    let session: Option<&Path> = if owned.as_os_str().is_empty() {
        None
    } else {
        Some(&owned)
    };

    // Window for crash reports: [start − slack, end + slack]. The slack
    // covers clock skew between the filename timestamp and the report's
    // mtime, and reports written a moment after the session file's last
    // append. Without a parseable start we still cap by the session mtime.
    let window: Option<(std::time::SystemTime, std::time::SystemTime)> = session.and_then(|s| {
        let mtime = std::fs::metadata(s).ok().and_then(|m| m.modified().ok())?;
        let start = crate::game_logs::session_started_at(s)
            .map(|t| t - std::time::Duration::from_secs(5))
            .unwrap_or(std::time::UNIX_EPOCH);
        let end = mtime + std::time::Duration::from_secs(5);
        Some((start, end))
    });

    let report_path = newest_crash_report(&mc_dir.join("crash-reports"), window);

    if let Some(path) = &report_path {
        if let Some(text) = read_report(path) {
            if let Some((kind, verdict, detail, culprit)) = classify_run_crash(&text) {
                return Ok(CrashAnalysis {
                    verdict: verdict.to_string(),
                    kind,
                    culprit,
                    detail,
                    source: "crash-report".into(),
                    report_path: Some(path.to_string_lossy().to_string()),
                });
            }
            // A report exists but matches nothing known: still hand the
            // user the file, with an honest "unknown" verdict.
            return Ok(CrashAnalysis {
                verdict: "unknown".into(),
                kind: "unknown".into(),
                culprit: None,
                detail: first_line(&text),
                source: "crash-report".into(),
                report_path: Some(path.to_string_lossy().to_string()),
            });
        }
    }

    // No crash report for this run — read the run's own unified log (full
    // stdout/stderr stream; catches early startup failures that never reach
    // a crash report).
    let session_text = session.and_then(|s| read_tail(s, MAX_LOG_TAIL_BYTES));

    // A confirmed clean exit (code 0) with no crash report in the window is
    // not a crash. This gate MUST run before the log classification: healthy
    // sessions routinely contain benign WARN noise ("Error loading class: …
    // ClassNotFoundException" for optional compat hooks) that would
    // otherwise be reported as a possible cause. A real startup failure
    // exits non-zero and is still classified below.
    if let Some(text) = &session_text {
        if exit_code_from_session_log(text) == Some(0) {
            return Ok(CrashAnalysis {
                verdict: "unknown".into(),
                kind: "clean".into(),
                culprit: None,
                detail: String::new(),
                source: "clean".into(),
                report_path: None,
            });
        }
    }

    if let Some(text) = &session_text {
        if let Some((kind, verdict, detail, culprit)) = classify_run_crash(text) {
            return Ok(CrashAnalysis {
                verdict: verdict.to_string(),
                kind,
                culprit,
                detail,
                source: "session-log".into(),
                report_path: None,
            });
        }
    }

    // Last resort: `latest.log` tail — only when it plausibly belongs to
    // the analyzed run (mtime inside the window), else it's a newer run's
    // file and would mislead.
    let latest = mc_dir.join("logs").join("latest.log");
    let latest_is_current = window
        .map(|(from, to)| {
            std::fs::metadata(&latest)
                .ok()
                .and_then(|m| m.modified().ok())
                .map(|m| m >= from && m <= to)
                .unwrap_or(false)
        })
        // No window (foreign session name): keep the legacy behavior.
        .unwrap_or(true);
    if latest_is_current {
        if let Some(text) = read_tail(&latest, MAX_LOG_TAIL_BYTES) {
            if let Some((kind, verdict, detail, culprit)) = classify_run_crash(&text) {
                return Ok(CrashAnalysis {
                    verdict: verdict.to_string(),
                    kind,
                    culprit,
                    detail,
                    source: "latest-log".into(),
                    report_path: None,
                });
            }
        }
    }

    Ok(CrashAnalysis {
        verdict: "unknown".into(),
        kind: "unknown".into(),
        culprit: None,
        detail: String::new(),
        source: "none".into(),
        report_path: None,
    })
}

fn first_line(text: &str) -> String {
    text.lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .map(|l| l.chars().take(400).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classify(text: &str) -> Option<(&'static str, &'static str, Option<String>)> {
        classify_crash(text).map(|(rule, _, culprit)| (rule.kind, rule.verdict, culprit))
    }

    /// `classify_run_crash` without the detail, for the refinement tests.
    fn classify_run(text: &str) -> Option<(String, &'static str, Option<String>)> {
        classify_run_crash(text).map(|(kind, verdict, _, culprit)| (kind, verdict, culprit))
    }

    #[test]
    fn oom_is_likely() {
        let text = "java.lang.OutOfMemoryError: Java heap space\n\tat net.minecraft.client.main.Main.main(Main.java:100)";
        let (kind, verdict, _) = classify(text).expect("oom should classify");
        assert_eq!(kind, "oom");
        assert_eq!(verdict, "likely");
    }

    #[test]
    fn mixin_failure_names_the_mod() {
        let text = "org.spongepowered.asm.mixin.transformer.throwables.MixinApplyError: Mixin [.mixins.json:foo] from mod [sodium] failed during APPLY step";
        let (kind, verdict, culprit) = classify(text).expect("mixin should classify");
        assert_eq!(kind, "mod_crash");
        assert_eq!(verdict, "likely");
        assert_eq!(culprit.as_deref(), Some("sodium"));
    }

    #[test]
    fn failed_loading_mod_extracts_quoted_name() {
        let text =
            "net.minecraftforge.fml.loading.FMLLoader: Failed loading mod file 'create-1.20.1.jar'";
        let (kind, _, culprit) = classify(text).expect("fml should classify");
        assert_eq!(kind, "mod_crash");
        assert_eq!(culprit.as_deref(), Some("create-1.20.1.jar"));
    }

    #[test]
    fn missing_dependency_is_likely() {
        let text = "Incompatible mod set!\nNo required mods found!\n• fabricloader >= 0.15.0 (fabricloader is missing)";
        // "fabricloader is missing)" doesn't contain "but it's missing",
        // but "Incompatible mod set!" does match the rule.
        let (kind, verdict, _) = classify(text).expect("deps should classify");
        assert_eq!(kind, "missing_dependency");
        assert_eq!(verdict, "likely");
    }

    #[test]
    fn fabric_incompatible_mods_found_is_loader_outdated() {
        // The owner-reported failure: Fabric Language Kotlin demands a newer
        // loader, the game dies at mod resolution and never writes a crash
        // report — the analyzer must read this straight from the run log.
        let text = "[08:20:58] [main/INFO]: Immediate reason: [HARD_DEP_NO_CANDIDATE fabric-language-kotlin 1.14.2+kotlin.2.4.21 {depends fabricloader @ [>=0.19.5]}, ROOT_FORCELOAD_SINGLE fabric-language-kotlin 1.14.2+kotlin.2.4.21]\n\
                    [08:20:58] [main/INFO]: Reason: [HARD_DEP fabric-language-kotlin 1.14.2+kotlin.2.4.21 {depends fabricloader @ [>=0.19.5]}]\n\
                    [08:20:58] [main/INFO]: Fix: add [], remove [], replace [[fabricloader 0.19.3] -> add:fabricloader 0.19.5 ([[0.19.5,\u{221e})])]\n\
                    [08:20:58] [main/ERROR]: Incompatible mods found!\n\
                    net.fabricmc.loader.impl.FormattedException: Some of your mods are incompatible with the game or each other!\n\
                    \t - Replace mod 'Fabric Loader' (fabricloader) 0.19.3 with version 0.19.5 or later.\n\
                    \t - Mod 'Fabric Language Kotlin' (fabric-language-kotlin) 1.14.2+kotlin.2.4.21 requires version 0.19.5 or later of mod 'Fabric Loader' (fabricloader), but only the wrong version is present: 0.19.3!";
        let (kind, verdict, detail, culprit) =
            classify_run_crash(text).expect("fabric should classify");
        assert_eq!(kind, "loader_outdated");
        assert_eq!(verdict, "likely");
        assert!(
            detail.contains("Replace mod 'Fabric Loader'"),
            "detail: {detail}"
        );
        assert_eq!(culprit.as_deref(), Some("Fabric Language Kotlin"));
    }

    #[test]
    fn fabric_incompatible_mods_without_loader_blame_stays_generic() {
        // A mod clash that has nothing to do with the loader version keeps
        // the generic kind — never a wrong "update the loader" verdict.
        let text = "[08:20:58] [main/ERROR]: Incompatible mods found!\n\
                    net.fabricmc.loader.impl.FormattedException: Some of your mods are incompatible with the game or each other!\n\
                    \t - Mod 'Cool Mod' (coolmod) 1.0.0 requires any version of fabric-api, which is missing!";
        let (kind, verdict, _) = classify_run(text).expect("fabric should classify");
        assert_eq!(kind, "mod_dependency");
        assert_eq!(verdict, "likely");
    }

    #[test]
    fn loader_mention_without_upgrade_lines_stays_generic() {
        let text = "fabricloader is present but the mod set failed\n[08:20:58] [main/ERROR]: Incompatible mods found!";
        let (kind, _, _) = classify_run(text).expect("fabric should classify");
        assert_eq!(kind, "mod_dependency");
    }

    #[test]
    fn java_version_mismatch() {
        let text = "Error: LinkageError occurred while loading main class net.minecraft.client.main.Main\njava.lang.UnsupportedClassVersionError: has been compiled by a more recent version of the Java Runtime";
        let (kind, verdict, _) = classify(text).expect("java should classify");
        assert_eq!(kind, "java_version");
        assert_eq!(verdict, "likely");
    }

    #[test]
    fn missing_class_with_jar_culprit() {
        let text = "Caused by: java.lang.ClassNotFoundException: com.example.Foo\n\tat java.base/jdk.internal.loader.BuiltinClassLoader.loadClass(BuiltinClassLoader.java:641)\n\tat cpw.mods.modlauncher... (mods/extrautils-1.0.jar)";
        let (kind, verdict, culprit) = classify(text).expect("class should classify");
        assert_eq!(kind, "missing_class");
        assert_eq!(verdict, "possible");
        assert_eq!(culprit.as_deref(), Some("extrautils-1.0.jar"));
    }

    #[test]
    fn unknown_text_does_not_classify() {
        assert!(classify("Everything worked fine and the game stopped normally.").is_none());
    }

    #[test]
    fn warn_noise_is_not_missing_class_evidence() {
        // Forge/NeoForge probe optional compat hooks and log this benign
        // WARN on perfectly healthy launches.
        let text = "[19:12:03] [main/WARN]: Error loading class: ca/spottedleaf/starlight/common/light/StarLightInterface$1 (java.lang.ClassNotFoundException: ca.spottedleaf.starlight.common.light.StarLightInterface$1)\n[19:12:05] [Render thread/INFO]: Sound engine started";
        assert!(classify(text).is_none());
    }

    #[test]
    fn fatal_missing_class_beats_warn_noise() {
        let text = "[19:12:03] [main/WARN]: Error loading class: opt.probe (java.lang.ClassNotFoundException: opt.probe)\n[19:12:10] [main/FATAL]: Caused by: java.lang.ClassNotFoundException: com.example.Real";
        let (kind, verdict, _) = classify(text).expect("fatal should classify");
        assert_eq!(kind, "missing_class");
        assert_eq!(verdict, "possible");
    }

    #[test]
    fn culprit_scan_prefers_the_evidence_neighborhood() {
        // The decoy jar line sits above the failure inside the context
        // window; an unrelated line must not win over the stack frame that
        // actually names the failing jar.
        let mut text = String::from("[12:00:00] [main/INFO]: Old listing (mods/decoy.jar)\n");
        for i in 0..10 {
            text.push_str(&format!("[12:00:0{i}] [main/INFO]: Filler line {i}\n"));
        }
        text.push_str("java.lang.OutOfMemoryError: Java heap space\n");
        text.push_str("\tat net.foo.Bar.baz (mods/realcause.jar)\n");
        let (_, _, culprit) = classify_crash(&text).expect("oom should classify");
        assert_eq!(culprit.as_deref(), Some("realcause.jar"));
    }

    #[test]
    fn detail_line_is_extracted_from_evidence() {
        let text = "some header line\njava.lang.OutOfMemoryError: Java heap space\nmore lines";
        let (_, detail, _) = classify_crash(text).expect("should classify");
        assert!(detail.contains("OutOfMemoryError"));
    }

    #[test]
    fn culprit_extraction_survives_non_ascii_before_match() {
        let text = "Сообщение об ошибке: Mixin apply failed from mod [sodium] because reasons";
        let (_, _, culprit) = classify_crash(text).expect("should classify");
        assert_eq!(culprit.as_deref(), Some("sodium"));
    }

    #[test]
    fn instance_without_crash_files_reports_none() {
        let dir = std::env::temp_dir().join(format!("vl_diag_{}", std::process::id()));
        std::fs::create_dir_all(dir.join("crash-reports")).unwrap();
        assert!(newest_crash_report(&dir.join("crash-reports"), None).is_none());
        std::fs::write(
            dir.join("crash-reports")
                .join("crash-2026-01-01_00.00.00.txt"),
            b"boom",
        )
        .unwrap();
        assert!(newest_crash_report(&dir.join("crash-reports"), None).is_some());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn crash_report_window_excludes_files_outside_the_run() {
        let dir = std::env::temp_dir().join(format!("vl_diag_win_{}", std::process::id()));
        std::fs::create_dir_all(dir.join("crash-reports")).unwrap();
        let old = dir
            .join("crash-reports")
            .join("crash-2020-01-01_00.00.00.txt");
        std::fs::write(&old, b"old boom").unwrap();
        // Window strictly after the file's mtime: the stale report must not
        // be attributed to the fresh run.
        let after = std::time::SystemTime::now() + std::time::Duration::from_secs(3600);
        assert!(newest_crash_report(&dir.join("crash-reports"), Some((after, after))).is_none());
        // Unfiltered lookup still finds it.
        assert!(newest_crash_report(&dir.join("crash-reports"), None).is_some());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn exit_code_is_parsed_from_the_final_session_line() {
        let text = "launch lines...\n[INFO] [launch] Game exited with code 1\n";
        assert_eq!(exit_code_from_session_log(text), Some(1));
        assert_eq!(
            exit_code_from_session_log("Game exited with code 0"),
            Some(0)
        );
        assert_eq!(exit_code_from_session_log("no exit line here"), None);
        // Last occurrence wins.
        let text = "Game exited with code 1\n...\nGame exited with code 0\n";
        assert_eq!(exit_code_from_session_log(text), Some(0));
    }

    #[test]
    fn gz_report_is_readable() {
        let dir = std::env::temp_dir().join(format!("vl_diag_gz_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("old.txt.gz");
        use std::io::Write;
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(b"java.lang.OutOfMemoryError: Java heap space")
            .unwrap();
        let bytes = enc.finish().unwrap();
        std::fs::write(&path, bytes).unwrap();
        let text = read_report(&path).expect("gz read");
        assert!(text.contains("OutOfMemoryError"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn failed_loading_mod_with_trailing_delimiter_terminates() {
        // Regression: the phrase is followed immediately by ",)" — the
        // scanner used to spin forever because the cursor never advanced
        // past the delimiter.
        let text = "Failed loading mod,) the game crashed";
        let culprit = extract_culprit(text, None);
        assert_eq!(culprit.as_deref(), Some("the"));
        let text = "Failed loading mod,,,'' words here";
        let culprit = extract_culprit(text, None);
        assert_eq!(culprit.as_deref(), Some("words"));
        // Phrase at the very end of the text must not loop either.
        assert_eq!(extract_culprit("Failed loading mod,", None), None);
    }
}

//! User-facing diagnostics (roadmap stage 6): system inventory, automatic
//! common-problem detection for a selected instance, and the crash analyzer.
//!
//! `run_diagnostics` is read-only: it inspects the OS/CPU/RAM/GPU/disk/Java,
//! probes reachability of the download services, and — when an instance is
//! selected — reuses the offline verify scan plus instance settings to flag
//! the usual launch blockers (missing loader, Java mismatch, over-allocated
//! memory, broken launcher files, invalid content archives).
//!
//! `analyze_crash` reads the newest crash report (or the tail of
//! `latest.log` when there is none) and classifies the failure into a
//! stable kind with a confidence verdict, so the UI can show a likely cause
//! instead of a raw log.

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
    ("mojang", "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json"),
    ("modrinth", "https://api.modrinth.com/v2/search?limit=1"),
    ("curseforge", "https://api.curseforge.com/"),
];

/// Probe the download services. Any HTTP response (even 4xx) means the
/// service is reachable; only transport errors/timeouts count as down.
async fn check_network() -> (usize, Vec<String>) {
    let Ok(client) = crate::download::global_http_client() else {
        return (0, NETWORK_HOSTS.iter().map(|(n, _)| n.to_string()).collect());
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
    let instance =
        instances::get_instance(&config.instances_dir(), instance_name).map_err(|e| e.to_string())?;

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
        let custom = instance.java_path.clone().or_else(|| config.java_path.clone());
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
pub async fn run_diagnostics(
    config: AppConfig,
    instance_name: Option<String>,
) -> DiagReport {
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
        let (sys_res, inst_res, net_res) = futures::future::join3(sys_task, inst_task, net_task).await;
        (
            sys_res.unwrap_or_else(|_| SystemInfo { checks: Vec::new(), issues: Vec::new() }),
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
    /// `"crash-report"` | `"latest-log"` | `"none"`.
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
        needles: &["nosuchmethoderror", "nosuchfielderror", "abstractmethoderror"],
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

fn line_containing(original: &str, needles: &[&str]) -> Option<String> {
    for line in original.split('\n') {
        let line_low = line.to_ascii_lowercase();
        if needles.iter().any(|n| line_low.contains(n)) {
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.chars().take(400).collect());
            }
        }
    }
    None
}

/// Pull a mod/file name out of the evidence text. Strategies are ordered
/// from most to least specific; all are plain substring scans.
fn extract_culprit(original: &str) -> Option<String> {
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
            let word = &tail[..end];
            cursor += end;
            if word.is_empty() || word.eq_ignore_ascii_case("file") || word.eq_ignore_ascii_case("mod")
            {
                continue;
            }
            return Some(word.chars().take(80).collect());
        }
    }

    // A jar path on the failing line: "… mods/sodium.jar"
    for line in original.split('\n') {
        let line_low = line.to_ascii_lowercase();
        if line_low.contains(".jar") && (line_low.contains("mods/") || line_low.contains("mods\\")) {
            let start = line_low.rfind(['/', '\\'])? + 1;
            let end = line_low[start..].find(".jar").map(|e| start + e + 4)?;
            let name = line[start..end].trim();
            if !name.is_empty() {
                return Some(name.chars().take(120).collect());
            }
        }
    }

    None
}

/// Classify a text blob (crash report or log tail). Returns the matched
/// rule, the evidence line, and the extracted culprit.
fn classify_crash(text: &str) -> Option<(&'static Rule, String, Option<String>)> {
    let low = text.to_ascii_lowercase();
    for rule in RULES {
        if rule.needles.iter().any(|n| find_ci(&low, n).is_some()) {
            let detail = line_containing(text, rule.needles).unwrap_or_default();
            let culprit = extract_culprit(text);
            return Some((rule, detail, culprit));
        }
    }
    None
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
fn newest_crash_report(crash_dir: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(crash_dir).ok()?;
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_lowercase();
        if !(name.ends_with(".txt") || name.ends_with(".txt.gz")) {
            continue;
        }
        let mtime = entry.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
        if best.as_ref().map(|(t, _)| mtime > *t).unwrap_or(true) {
            best = Some((mtime, path));
        }
    }
    best.map(|(_, p)| p)
}

/// Analyze the most recent crash of an instance: newest crash report
/// first, then the tail of `latest.log`. Classifies the failure into a
/// stable kind + confidence so the UI can show a likely cause.
pub fn analyze_crash(config: &AppConfig, instance_name: &str) -> Result<CrashAnalysis, String> {
    let instance =
        instances::get_instance(&config.instances_dir(), instance_name).map_err(|e| e.to_string())?;
    let mc_dir = instance.minecraft_dir(&config.instances_dir());

    let report_path = newest_crash_report(&mc_dir.join("crash-reports"));

    if let Some(path) = &report_path {
        if let Some(text) = read_report(path) {
            if let Some((rule, detail, culprit)) = classify_crash(&text) {
                return Ok(CrashAnalysis {
                    verdict: rule.verdict.to_string(),
                    kind: rule.kind.to_string(),
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

    // No crash report — the log tail may still say what happened
    // (e.g. early startup failures never reach a crash report).
    if let Some(text) = read_tail(&mc_dir.join("logs").join("latest.log"), MAX_LOG_TAIL_BYTES) {
        if let Some((rule, detail, culprit)) = classify_crash(&text) {
            return Ok(CrashAnalysis {
                verdict: rule.verdict.to_string(),
                kind: rule.kind.to_string(),
                culprit,
                detail,
                source: "latest-log".into(),
                report_path: None,
            });
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
        let text = "net.minecraftforge.fml.loading.FMLLoader: Failed loading mod file 'create-1.20.1.jar'";
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
        assert!(newest_crash_report(&dir.join("crash-reports")).is_none());
        std::fs::write(dir.join("crash-reports").join("crash-2026-01-01_00.00.00.txt"), b"boom").unwrap();
        assert!(newest_crash_report(&dir.join("crash-reports")).is_some());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn gz_report_is_readable() {
        let dir = std::env::temp_dir().join(format!("vl_diag_gz_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("old.txt.gz");
        use std::io::Write;
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(b"java.lang.OutOfMemoryError: Java heap space").unwrap();
        let bytes = enc.finish().unwrap();
        std::fs::write(&path, bytes).unwrap();
        let text = read_report(&path).expect("gz read");
        assert!(text.contains("OutOfMemoryError"));
        std::fs::remove_dir_all(&dir).ok();
    }
}

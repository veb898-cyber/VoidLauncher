//! Pure crash-evidence parsing.
//!
//! `diagnostics` owns the run window and the report files; this module only
//! turns text into a conservative explanation. It never claims a mod is guilty
//! from a bare name mention: loader requirement blocks, fatal mixin failures,
//! crash-report `Suspected Mods` and native JVM frames are the only signals
//! that can name one.

/// One explanation extracted from a log. `kind` maps to the frontend i18n
/// (`diag.crash_<kind>`); `file` is a mod jar only when the log named one.
#[derive(Debug, Clone, PartialEq)]
pub struct CrashSignal {
    pub kind: &'static str,
    pub verdict: &'static str,
    pub detail: String,
    pub culprit: Option<String>,
    pub file: Option<String>,
}

/// Whether a line is a level header that the game survived. Requirement
/// listings under an ERROR header are not benign; indented continuation lines
/// inherit the last level.
fn level_of(line: &str) -> Option<bool> {
    let t = line.trim_start();
    if !t.starts_with('[') {
        return None;
    }
    let head: String = t.chars().take(180).collect::<String>().to_lowercase();
    for marker in [
        "/warn]", "/info]", "/debug]", "/trace]", "[warn", "[info", "[debug",
    ] {
        if head.contains(marker) {
            return Some(true);
        }
    }
    for marker in ["/error]", "/fatal]", "[error", "[fatal", "[severe"] {
        if head.contains(marker) {
            return Some(false);
        }
    }
    None
}

fn is_underline(line: &str) -> bool {
    let t = line.trim();
    !t.is_empty() && t.chars().all(|c| matches!(c, '-' | '=' | '\u{2500}' | ' '))
}

/// The first `'…'` name and an optional `(id)` immediately after it.
fn quoted_name(line: &str) -> Option<(String, Option<String>)> {
    let start = line.find('\'')? + 1;
    let end = line[start..].find('\'')? + start;
    let name = line[start..end].trim();
    if name.is_empty() || name.len() > 100 {
        return None;
    }
    let after = line[end + 1..].trim_start();
    let id = after
        .strip_prefix('(')
        .and_then(|rest| rest.split_once(')'))
        .map(|(id, _)| id.trim().to_string())
        .filter(|id| !id.is_empty() && id.len() <= 80 && !id.contains(' '));
    Some((name.to_string(), id))
}

fn mod_ref(line: &str) -> Option<String> {
    quoted_name(line)
        .map(|(name, id)| id.unwrap_or(name))
        .or_else(|| {
            let t = line.trim_start();
            let t = t.strip_prefix("mod ").unwrap_or(t);
            let token = t
                .split(|c: char| c.is_whitespace() || matches!(c, ',' | '!' | ';' | ')'))
                .next()?
                .trim_matches(['\'', '"']);
            (!token.is_empty() && token.len() <= 80).then(|| token.to_string())
        })
}

fn first_token_after<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let at = line.find(key)? + key.len();
    line[at..]
        .split(|c: char| c.is_whitespace() || matches!(c, ',' | '!' | ';' | ')' | ']'))
        .find(|s| !s.is_empty())
        .map(str::trim)
}

fn file_name(path: &str) -> String {
    path.trim()
        .trim_matches(['\'', '"'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(path)
        .to_string()
}

fn detail(line: &str) -> String {
    line.trim().chars().take(400).collect()
}

fn fabric_requirement(line: &str) -> Option<CrashSignal> {
    let low = line.to_lowercase();
    if low.contains(" recommends ") || !low.contains(',') {
        return None;
    }
    let verb = [" is incompatible with ", " breaks with ", " requires "]
        .into_iter()
        .find(|v| line.contains(*v))?;
    let at = line.find(verb)?;
    let by = mod_ref(&line[..at])?;
    let rest = &line[at + verb.len()..];
    if verb != " requires " {
        let other = mod_ref(rest)?;
        return Some(CrashSignal {
            kind: "mod_conflict",
            verdict: "possible",
            detail: detail(line),
            culprit: Some(other),
            file: None,
        });
    }
    let of = rest.find(" of ")?;
    let dep = mod_ref(&rest[of + 4..])?;
    let dep_low = dep.to_lowercase();
    let loader = matches!(
        dep_low.as_str(),
        "fabricloader"
            | "fabric-loader"
            | "quilt_loader"
            | "quilt-loader"
            | "forge"
            | "neoforge"
            | "javafml"
            | "fml"
    );
    let missing = low.contains("which is missing")
        || low.contains("but it's missing")
        || low.contains("but it's not installed");
    let wrong_version =
        low.contains("but only the wrong version is present") || low.contains("present: ");
    let kind = if loader {
        "loader_outdated"
    } else if missing {
        "missing_dependency"
    } else if wrong_version {
        "mod_version_mismatch"
    } else {
        return None;
    };
    Some(CrashSignal {
        kind,
        verdict: "likely",
        detail: detail(line),
        culprit: Some(by),
        file: None,
    })
}

fn forge_requirement(line: &str) -> Option<CrashSignal> {
    if !line.contains("Mod ID:") || !line.contains("Requested by:") {
        return None;
    }
    let field = |key: &str| -> Option<String> {
        let rest = &line[line.find(key)? + key.len()..];
        let start = rest.find('\'')? + 1;
        let end = rest[start..].find('\'')? + start;
        Some(rest[start..end].trim().to_string())
    };
    let dep = field("Mod ID:")?;
    let by = field("Requested by:")?;
    let actual = field("Actual version:").unwrap_or_default();
    let missing = actual.is_empty()
        || actual.eq_ignore_ascii_case("[missing]")
        || actual.eq_ignore_ascii_case("null");
    let dep_low = dep.to_lowercase();
    let loader = matches!(
        dep_low.as_str(),
        "fabricloader"
            | "fabric-loader"
            | "quilt_loader"
            | "quilt-loader"
            | "forge"
            | "neoforge"
            | "javafml"
            | "fml"
    );
    Some(CrashSignal {
        kind: if loader {
            "loader_outdated"
        } else if missing {
            "missing_dependency"
        } else {
            "mod_version_mismatch"
        },
        verdict: "likely",
        detail: detail(line),
        culprit: Some(by),
        file: None,
    })
}

fn wrong_loader(line: &str) -> Option<CrashSignal> {
    let at = line.find("File ")?;
    let rest = &line[at + 5..];
    let (marker, loader) = [
        (" is a Fabric mod and cannot be loaded", "Fabric"),
        (" is a Quilt mod and cannot be loaded", "Quilt"),
        (" is for Minecraft Forge", "Forge"),
        (" is for an old version of Minecraft Forge", "Forge"),
        (" is a LiteLoader mod", "LiteLoader"),
    ]
    .into_iter()
    .find(|(m, _)| rest.contains(m))?;
    let path = &rest[..rest.find(marker)?];
    let file = file_name(path);
    (!file.is_empty()).then(|| CrashSignal {
        kind: "wrong_loader",
        verdict: "likely",
        detail: format!("{} ({})", detail(line), loader),
        culprit: Some(file.clone()),
        file: Some(file),
    })
}

fn wrong_game(line: &str) -> Option<CrashSignal> {
    let low = line.to_lowercase();
    let file = if low.contains("missing language") {
        let at = low.find(" wanted by ")?;
        file_name(line[at + 11..].split(',').next().unwrap_or(""))
    } else if low.contains("needs language provider") {
        let at = line.find("Mod File ")?;
        file_name(
            line[at + 9..]
                .split(" needs language provider")
                .next()
                .unwrap_or(""),
        )
    } else {
        return None;
    };
    (!file.is_empty() && file.to_lowercase().ends_with(".jar")).then(|| CrashSignal {
        kind: "wrong_game",
        verdict: "likely",
        detail: detail(line),
        culprit: Some(file.clone()),
        file: Some(file),
    })
}

fn duplicate_mod(line: &str) -> Option<CrashSignal> {
    let (id, list) = if let Some(at) = line.find(" from mod files: ") {
        let id = line[..at].rsplit('\'').next().unwrap_or("").to_lowercase();
        (id, &line[at + 17..])
    } else if let Some(at) = line.find(" is present in multiple files: ") {
        let id = line[..at]
            .split_whitespace()
            .last()
            .unwrap_or("")
            .to_lowercase();
        (id, &line[at + 31..])
    } else {
        return None;
    };
    let files: Vec<String> = list
        .split(',')
        .map(file_name)
        .filter(|f| !f.is_empty())
        .collect();
    if id.is_empty() {
        return None;
    }
    let culprit = files.first().cloned().or(Some(id));
    Some(CrashSignal {
        kind: "duplicate_mod",
        verdict: "likely",
        detail: detail(line),
        culprit,
        file: None,
    })
}

fn mixin_owner(line: &str) -> Option<CrashSignal> {
    let low = line.to_lowercase();
    if ![
        "mixin apply failed",
        "mixinapplyerror",
        "mixintransformererror",
        "invalidinjectionexception",
        "mixin application of",
    ]
    .iter()
    .any(|m| low.contains(m))
    {
        return None;
    }
    let owner = first_token_after(&low, "mixin apply for mod ")
        .or_else(|| first_token_after(&low, " from mod "))
        .map(str::to_string)
        .or_else(|| {
            low.find(".mixins.json").and_then(|at| {
                let head = &low[..at];
                let start = head
                    .rfind(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')))
                    .map(|i| i + 1)
                    .unwrap_or(0);
                let id = head[start..].split(['.', '-']).next().unwrap_or("");
                (!id.is_empty()).then(|| id.to_string())
            })
        })?;
    Some(CrashSignal {
        kind: "mod_crash",
        verdict: "likely",
        detail: detail(line),
        culprit: Some(owner),
        file: None,
    })
}

fn suspected_mod(line: &str) -> Option<CrashSignal> {
    let trimmed = line.trim();
    let low = trimmed.to_lowercase();
    let entry = if let Some(rest) = trimmed.strip_prefix("Suspected Mods:") {
        rest.split(',').next().unwrap_or("").trim()
    } else if let Some(rest) = low.strip_prefix("-- mod loading issue for:") {
        return Some(CrashSignal {
            kind: "mod_crash",
            verdict: "likely",
            detail: detail(line),
            culprit: Some(
                rest.trim()
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
                    .collect(),
            ),
            file: None,
        });
    } else {
        return None;
    };
    let open = entry.find(" (")?;
    let close = open + 2 + entry[open + 2..].find(')')?;
    let id = entry[open + 2..close].trim().to_lowercase();
    if id.is_empty() || id.contains(' ') || id == "minecraft" || id == "java" {
        return None;
    }
    Some(CrashSignal {
        kind: "mod_crash",
        verdict: "likely",
        detail: detail(line),
        culprit: Some(id),
        file: None,
    })
}

fn loader_signal(text: &str) -> Option<CrashSignal> {
    let mut benign = false;
    for line in text.lines() {
        if line.trim().is_empty() || is_underline(line) {
            continue;
        }
        if let Some(level) = level_of(line) {
            benign = level;
        }
        // Crash-report suspect sections are not log levels and must be read
        // before the benign gate.
        if let Some(sig) = suspected_mod(line) {
            return Some(sig);
        }
        if benign {
            continue;
        }
        if let Some(mut sig) = wrong_loader(line)
            .or_else(|| wrong_game(line))
            .or_else(|| duplicate_mod(line))
            .or_else(|| forge_requirement(line))
            .or_else(|| fabric_requirement(line))
            .or_else(|| mixin_owner(line))
        {
            // For a loader-version failure the solver's "Replace … with …"
            // sentence is more actionable than the requirement sentence that
            // follows it, so prefer that line when the log contains one.
            if sig.kind == "loader_outdated" {
                if let Some(replace) = text.lines().find(|l| {
                    let l = l.to_lowercase();
                    (l.contains("replace mod 'fabric loader'")
                        || l.contains("replace mod \"fabric loader\""))
                        && l.contains("or later")
                }) {
                    sig.detail = detail(replace);
                }
            }
            return Some(sig);
        }
    }
    None
}

fn native_signal(text: &str) -> Option<CrashSignal> {
    let low = text.to_lowercase();
    if !low.contains("exception_access_violation")
        && !low.contains("sigsegv")
        && !low.contains("exception_illegal_instruction")
        && !low.contains("# problematic frame:")
    {
        return None;
    }
    let detail_line = text
        .lines()
        .find(|l| {
            let l = l.trim_start_matches('#').trim();
            l.starts_with("EXCEPTION_") || l.starts_with("SIG") || l.starts_with("Internal Error")
        })
        .or_else(|| text.lines().find(|l| l.contains("# Problematic frame:")))
        .map(detail)
        .unwrap_or_default();
    let module = text
        .lines()
        .find(|l| {
            let t = l.trim_start_matches('#').trim_start();
            t.starts_with("C  [") || t.starts_with("J  [")
        })
        .and_then(|l| l.split('[').nth(1))
        .and_then(|s| s.split(['+', ']']).next())
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty());
    let driver = module.as_deref().and_then(|m| {
        if m.contains("nvoglv") || m.contains("nvcuda") {
            Some("nvidia")
        } else if m.contains("atio") || m.contains("amd") {
            Some("amd")
        } else if m.contains("igx") || m.contains("igdr") || m.contains("igd") {
            Some("intel")
        } else {
            None
        }
    });
    Some(CrashSignal {
        kind: if driver.is_some() {
            "graphics_driver"
        } else {
            "native_crash"
        },
        verdict: "likely",
        detail: detail_line,
        culprit: driver.map(str::to_string).or(module),
        file: None,
    })
}

fn system_memory_signal(text: &str) -> Option<CrashSignal> {
    let low = text.to_lowercase();
    let oom = low.contains("outofmemoryerror")
        || low.contains("out of memory")
        || low.contains("native memory allocation");
    if !oom {
        return None;
    }
    let system = low.contains("native memory")
        || low.contains("unable to create new native thread")
        || low.contains("paging file")
        || low.contains("not enough memory")
        || low.contains("failed to allocate");
    if !system {
        return None;
    }
    let line = text
        .lines()
        .find(|l| {
            let l = l.to_lowercase();
            l.contains("outofmemoryerror")
                || l.contains("out of memory")
                || l.contains("native memory")
        })
        .map(detail)
        .unwrap_or_default();
    Some(CrashSignal {
        kind: "system_memory",
        verdict: "likely",
        detail: line,
        culprit: None,
        file: None,
    })
}

/// The strongest non-generic explanation in the text, if any.
pub fn analyze_text(text: &str) -> Option<CrashSignal> {
    system_memory_signal(text)
        .or_else(|| native_signal(text))
        .or_else(|| loader_signal(text))
}

/// A log can report a crash even when the process exits with code 0:
/// Minecraft catches the game-loop exception, logs it and lets `main` return.
/// Chat and named-entity text are excluded; Forge's stdout routing is kept.
pub fn reports_crash(text: &str) -> bool {
    const MARKERS: [&str; 8] = [
        "unhandled game exception",
        "reported exception thrown",
        "unreported exception thrown",
        "#@!@# game crashed",
        "shutdown failure",
        "minecraft has crashed",
        "---- minecraft crash report ----",
        "crash report saved to",
    ];
    text.lines().any(|line| {
        let low = line.to_ascii_lowercase();
        if low.contains("[chat]") || low.contains("named entity") {
            return false;
        }
        MARKERS.iter().any(|m| low.contains(m))
    })
}

/// Last `Caused by:` line, which is usually the actionable root cause.
pub fn root_cause(text: &str) -> Option<String> {
    let mut found = None;
    for line in text.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("Caused by:") {
            let value = rest.trim();
            if !value.is_empty() {
                found = Some(value.chars().take(400).collect());
            }
        }
    }
    found
}

/// Official vendor driver pages only; no arbitrary URL from a log can reach
/// the opener.
pub fn driver_url(text: &str) -> Option<&'static str> {
    let low = text.to_lowercase();
    if low.contains("nvoglv") || low.contains("nvidia") {
        Some("https://www.nvidia.com/Download/index.aspx")
    } else if low.contains("atio") || low.contains("amd") {
        Some("https://www.amd.com/en/support")
    } else if low.contains("igx") || low.contains("intel") || low.contains("igd") {
        Some("https://www.intel.com/content/www/us/en/download-center/home.html")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fabric_missing_dependency_is_named() {
        let log = "\t - Mod 'Mod Menu' (modmenu) 7.2.2 requires version 0.92.0 or later of fabric-api, which is missing!";
        let signal = analyze_text(log).expect("Fabric refusal should classify");
        assert_eq!(signal.kind, "missing_dependency");
        assert_eq!(signal.verdict, "likely");
        assert_eq!(signal.culprit.as_deref(), Some("modmenu"));
        assert!(signal.detail.contains("fabric-api"));
    }

    #[test]
    fn fabric_wrong_version_and_loader_are_distinguished() {
        let wrong = "\t - Mod 'Iris' (iris) 1.7.0 requires version 0.5.8 or later of mod 'Sodium' (sodium), but only the wrong version is present: 0.5.3!";
        assert_eq!(analyze_text(wrong).unwrap().kind, "mod_version_mismatch");

        let loader = "\t - Mod 'Iris' (iris) 1.7.0 requires version 0.16.0 or later of fabricloader, but only the wrong version is present: 0.14.21!";
        let signal = analyze_text(loader).unwrap();
        assert_eq!(signal.kind, "loader_outdated");
        assert_eq!(signal.culprit.as_deref(), Some("iris"));
    }

    #[test]
    fn forge_requirement_and_suspected_mod_are_supported() {
        let forge = "\tMod ID: 'craftedcore', Requested by: 'walkers', Expected range: '[5.8.1,)', Actual version: '[MISSING]'";
        let signal = analyze_text(forge).unwrap();
        assert_eq!(signal.kind, "missing_dependency");
        assert_eq!(signal.culprit.as_deref(), Some("walkers"));

        let suspected = "Suspected Mods: Create (create), Version: 6.0.4";
        let signal = analyze_text(suspected).unwrap();
        assert_eq!(signal.kind, "mod_crash");
        assert_eq!(signal.culprit.as_deref(), Some("create"));
    }

    #[test]
    fn wrong_loader_and_wrong_game_name_the_jar() {
        let loader = "File mods/sodium-1.20.1.jar is a Fabric mod and cannot be loaded";
        let signal = analyze_text(loader).unwrap();
        assert_eq!(signal.kind, "wrong_loader");
        assert_eq!(signal.file.as_deref(), Some("sodium-1.20.1.jar"));

        let game = "Mod File mods/old.jar needs language provider javafml:66 or above to load";
        let signal = analyze_text(game).unwrap();
        assert_eq!(signal.kind, "wrong_game");
        assert_eq!(signal.file.as_deref(), Some("old.jar"));
    }

    #[test]
    fn native_driver_and_heap_vs_system_memory_are_distinguished() {
        let native =
            "# Problematic frame:\n# C  [nvoglv64.dll+0x1234]\n# EXCEPTION_ACCESS_VIOLATION";
        let signal = analyze_text(native).unwrap();
        assert_eq!(signal.kind, "graphics_driver");
        assert!(driver_url(native).unwrap().contains("nvidia.com"));

        let heap = "java.lang.OutOfMemoryError: Java heap space";
        assert!(
            analyze_text(heap).is_none(),
            "heap OOM is handled by the generic rule"
        );

        let system = "java.lang.OutOfMemoryError: unable to create new native thread";
        assert_eq!(analyze_text(system).unwrap().kind, "system_memory");
    }

    #[test]
    fn a_clean_log_is_not_a_crash_signal() {
        let text = "[12:00:00] [Render thread/INFO]: Stopping!\n[12:00:01] [Render thread/INFO]: Saving worlds";
        assert!(analyze_text(text).is_none());
    }

    #[test]
    fn reports_crash_even_when_the_exit_code_is_zero() {
        assert!(reports_crash(
            "[09:46:03] [Render thread/ERROR]: Unhandled game exception\njava.lang.RuntimeException: boom"
        ));
        assert!(reports_crash(
            "[12:00:00] [Render thread/INFO] [STDOUT/]: #@!@# Game crashed! Crash report saved to: #@!@# crash.txt"
        ));
        assert!(!reports_crash(
            "[12:00:00] [Render thread/INFO]: Stopping!\n[12:00:01] [Render thread/INFO]: Saving worlds"
        ));
        assert!(!reports_crash(
            "[12:00:00] [Render thread/INFO]: [CHAT] <griefer> Minecraft has crashed, Reported exception thrown!"
        ));
    }

    #[test]
    fn root_cause_uses_the_last_caused_by() {
        let text = "java.lang.RuntimeException: outer\nCaused by: java.lang.IllegalStateException: middle\nCaused by: java.lang.NullPointerException: actual";
        assert_eq!(
            root_cause(text).as_deref(),
            Some("java.lang.NullPointerException: actual")
        );
    }
}

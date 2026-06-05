use std::path::Path;
use std::process::Command;

/// Supported GC preset strategies. Serialized as lowercase strings
/// in the Instance config: "standard" | "g1gc" | "zgc".
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GcPreset {
    /// No special GC flags. Caller is responsible for any flags they want.
    Standard,
    /// Aikar's Flags — universal, safe for Java 8/17/21.
    G1gcAikar,
    /// Modern ZGC — low pause times, requires Java 17+ and 6+ GB of heap.
    ModernZgc,
}

impl GcPreset {
    /// Parse from the lowercase string stored in the instance config.
    /// Unknown values fall back to G1gcAikar (the safe default).
    pub fn from_str(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "standard" | "std" | "" => GcPreset::Standard,
            "g1gc" | "g1" | "aikar" | "aikars" => GcPreset::G1gcAikar,
            "zgc" | "modern" | "modern-zgc" => GcPreset::ModernZgc,
            _ => GcPreset::G1gcAikar,
        }
    }
}

/// Run `java -version` against the given path and return the major version
/// (e.g. 8, 17, 21). Returns None if the path is invalid or unparseable.
pub fn detect_java_major(java_path: &Path) -> Option<u32> {
    let output = Command::new(java_path)
        .arg("-version")
        .output()
        .ok()?;
    // `java -version` writes to stderr
    let combined = if !output.stderr.is_empty() {
        String::from_utf8_lossy(&output.stderr).to_string()
    } else {
        String::from_utf8_lossy(&output.stdout).to_string()
    };
    Some(parse_java_major(&combined))
}

/// Parse the major version from `java -version` output.
/// Format examples:
///   openjdk version "17.0.9" 2023-10-17  -> 17
///   openjdk version "1.8.0_382"          -> 8
///   java version "21.0.1" 2023-10-17      -> 21
fn parse_java_major(output: &str) -> u32 {
    for line in output.lines() {
        if !line.contains("version") { continue; }
        let Some(start) = line.find('"') else { continue; };
        let rest = &line[start + 1..];
        let Some(end) = rest.find('"') else { continue; };
        let version = &rest[..end];
        let parts: Vec<&str> = version.split('.').collect();
        if let Some(first) = parts.first() {
            if let Ok(n) = first.parse::<u32>() {
                if n == 1 && parts.len() > 1 {
                    // Legacy 1.8.0_xxx format -> major is the second part
                    if let Ok(major) = parts[1].parse::<u32>() {
                        return major;
                    }
                }
                return n;
            }
        }
    }
    0
}

/// Build the JVM argument list for a given preset, memory budget, and Java major version.
///
/// Returns a tuple of (final_args, effective_preset). The effective preset may differ
/// from the requested one if safety rules forced a fallback (e.g. ZGC -> G1GC).
///
/// - Xms and Xmx are always set equal, using the supplied `memory_mb`.
/// - The flag list never includes the user's custom args; those should be appended
///   by the caller AFTER this function returns.
/// - ZGC requires Java >= 17 AND >= 6 GB of heap. If either condition fails,
///   the preset silently downgrades to G1GC and a warning is logged.
pub fn build_jvm_args(
    requested: GcPreset,
    memory_mb: u32,
    java_major: u32,
) -> (Vec<String>, GcPreset) {
    let mut args: Vec<String> = Vec::new();

    // Memory is set FIRST so subsequent flags operate on a known heap size.
    args.push(format!("-Xms{}M", memory_mb));
    args.push(format!("-Xmx{}M", memory_mb));

    let effective = match requested {
        GcPreset::Standard => GcPreset::Standard,
        GcPreset::G1gcAikar => GcPreset::G1gcAikar,
        GcPreset::ModernZgc => {
            if java_major < 17 {
                eprintln!(
                    "[JVM] ZGC requires Java 17+ (detected: {}). Falling back to G1GC.",
                    java_major
                );
                GcPreset::G1gcAikar
            } else if memory_mb < 6144 {
                eprintln!(
                    "[JVM] ZGC requires at least 6 GB of heap (allocated: {} MB). Falling back to G1GC.",
                    memory_mb
                );
                GcPreset::G1gcAikar
            } else {
                GcPreset::ModernZgc
            }
        }
    };

    match effective {
        GcPreset::Standard => {
            // No GC flags added by the preset; the caller's custom args
            // (or instance.jvm_args) provide whatever they want.
        }
        GcPreset::G1gcAikar => {
            // Universal Aikar's flags. Supported on Java 8, 11, 17, 21.
            args.push("-XX:+UseG1GC".into());
            args.push("-XX:+ParallelRefProcEnabled".into());
            args.push("-XX:MaxGCPauseMillis=50".into());
            args.push("-XX:+UnlockExperimentalVMOptions".into());
            // G1UnlockCommercialFeatures was removed in Java 9+; it only
            // exists in Java 8. Adding it on newer JDKs would error out.
            if java_major == 8 {
                args.push("-XX:+G1UnlockCommercialFeatures".into());
            }
        }
        GcPreset::ModernZgc => {
            args.push("-XX:+UseZGC".into());
            args.push("-XX:+UnlockExperimentalVMOptions".into());
        }
    }

    (args, effective)
}

/// Strip JVM flags that *select* a garbage collector from a list of args.
///
/// Mojang's modern version manifests (1.20.5+) ship `-XX:+UseG1GC` (or
/// occasionally `-XX:+UseZGC` on snapshots) in their `arguments.jvm`
/// block. The HotSpot JVM refuses to start with the error
/// "multiple garbage collectors selected" if two such flags coexist,
/// so when the launcher picks its own GC preset we MUST remove every
/// GC-selection flag from the upstream manifest first.
///
/// The launcher preset (chosen by the user) is authoritative — anything
/// pre-baked into the manifest or a loader profile is dropped here.
/// Flags that merely *tune* an already-selected GC (e.g.
/// `-XX:MaxGCPauseMillis=50`, `-XX:+UseStringDeduplication`) are kept
/// intact, as they don't conflict.
///
/// Returns a new vector; the input slice is not modified.
pub fn strip_gc_selection_flags(args: &[String]) -> Vec<String> {
    // Recognised GC selection flags (both `+` enable and `-` disable forms).
    // Kept in one place so adding a new GC later is a one-line change.
    const SELECTORS: &[&str] = &[
        "UseG1GC",
        "UseZGC",
        "UseParallelGC",
        "UseSerialGC",
        "UseConcMarkSweepGC",
        "UseShenandoahGC",
        "UseEpsilonGC",
    ];

    let mut out: Vec<String> = Vec::with_capacity(args.len());
    for arg in args {
        // Match `-XX:+UseXxx` and `-XX:-UseXxx` (case-sensitive, matching
        // the canonical flag form HotSpot itself accepts).
        if let Some(rest) = arg.strip_prefix("-XX:").or_else(|| arg.strip_prefix("-xx:")) {
            if rest.len() >= 2 {
                let (sign, name) = rest.split_at(1);
                if (sign == "+" || sign == "-") && SELECTORS.iter().any(|s| *s == name) {
                    continue;
                }
            }
        }
        out.push(arg.clone());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_modern_jdk() {
        assert_eq!(parse_java_major(r#"openjdk version "17.0.9" 2023-10-17"#), 17);
        assert_eq!(parse_java_major(r#"openjdk version "21.0.1" 2023-10-17"#), 21);
    }

    #[test]
    fn parse_legacy_jdk() {
        assert_eq!(parse_java_major(r#"openjdk version "1.8.0_382""#), 8);
    }

    #[test]
    fn parse_oracle() {
        assert_eq!(parse_java_major(r#"java version "21.0.1" 2023-10-17 LTS"#), 21);
    }

    #[test]
    fn g1gc_always_works() {
        let (args, eff) = build_jvm_args(GcPreset::G1gcAikar, 4096, 8);
        assert_eq!(eff, GcPreset::G1gcAikar);
        assert!(args.contains(&"-XX:+UseG1GC".to_string()));
        assert!(args.contains(&"-XX:+G1UnlockCommercialFeatures".to_string())); // Java 8
        assert!(args.iter().any(|a| a == "-Xms4096M"));
        assert!(args.iter().any(|a| a == "-Xmx4096M"));
    }

    #[test]
    fn g1gc_skips_commercial_flag_on_java_17() {
        let (args, _) = build_jvm_args(GcPreset::G1gcAikar, 4096, 17);
        assert!(!args.contains(&"-XX:+G1UnlockCommercialFeatures".to_string()));
    }

    #[test]
    fn zgc_falls_back_when_old_java() {
        let (_, eff) = build_jvm_args(GcPreset::ModernZgc, 8192, 11);
        assert_eq!(eff, GcPreset::G1gcAikar);
    }

    #[test]
    fn zgc_falls_back_when_low_memory() {
        let (_, eff) = build_jvm_args(GcPreset::ModernZgc, 4096, 21);
        assert_eq!(eff, GcPreset::G1gcAikar);
    }

    #[test]
    fn zgc_works_with_java_17_and_8gb() {
        let (args, eff) = build_jvm_args(GcPreset::ModernZgc, 8192, 21);
        assert_eq!(eff, GcPreset::ModernZgc);
        assert!(args.contains(&"-XX:+UseZGC".to_string()));
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn strip_removes_plus_g1gc() {
        let out = strip_gc_selection_flags(&s(&["-XX:+UseG1GC", "-Xmx4G"]));
        assert_eq!(out, s(&["-Xmx4G"]));
    }

    #[test]
    fn strip_removes_minus_g1gc() {
        let out = strip_gc_selection_flags(&s(&["-XX:-UseG1GC", "-Xms4G"]));
        assert_eq!(out, s(&["-Xms4G"]));
    }

    #[test]
    fn strip_removes_all_recognised_gcs() {
        let input = s(&[
            "-XX:+UseG1GC",
            "-XX:+UseZGC",
            "-XX:+UseParallelGC",
            "-XX:+UseSerialGC",
            "-XX:+UseConcMarkSweepGC",
            "-XX:+UseShenandoahGC",
            "-XX:+UseEpsilonGC",
        ]);
        let out = strip_gc_selection_flags(&input);
        assert!(out.is_empty(), "all GC selectors should be removed, got: {:?}", out);
    }

    #[test]
    fn strip_keeps_tuning_flags() {
        let out = strip_gc_selection_flags(&s(&[
            "-XX:MaxGCPauseMillis=50",
            "-XX:+UseStringDeduplication",
            "-XX:+ParallelRefProcEnabled",
        ]));
        assert_eq!(out.len(), 3, "tuning flags should be kept, got: {:?}", out);
    }

    #[test]
    fn strip_keeps_unrelated_args() {
        let out = strip_gc_selection_flags(&s(&[
            "-Xms4G",
            "-Xmx4G",
            "-Dfile.encoding=UTF-8",
            "-cp",
            "some.jar",
        ]));
        assert_eq!(out.len(), 5);
    }

    #[test]
    fn strip_accepts_lowercase_xx_prefix() {
        // Some launchers/tools pass `-xx:+UseG1GC` — we still strip it.
        let out = strip_gc_selection_flags(&s(&["-xx:+UseG1GC", "-Xmx4G"]));
        assert_eq!(out, s(&["-Xmx4G"]));
    }

    #[test]
    fn strip_does_not_match_substring() {
        // `-XX:+UseG1GCFoo` is NOT a real flag and must not be stripped.
        let out = strip_gc_selection_flags(&s(&["-XX:+UseG1GCFoo", "-Xmx4G"]));
        assert_eq!(out, s(&["-XX:+UseG1GCFoo", "-Xmx4G"]));
    }

    #[test]
    fn strip_empty_input() {
        let out = strip_gc_selection_flags(&[]);
        assert!(out.is_empty());
    }
}

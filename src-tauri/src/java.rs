use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

/// Windows creation flag to hide console windows
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

/// Run a command and collect its output, aborting with a timeout instead of
/// hanging forever. `std::process::Command::output()` blocks indefinitely —
/// a stuck java.exe (mid-update, broken install, locked by antivirus) would
/// otherwise freeze the launcher's Java probes and Forge install processors.
pub(crate) fn run_command_with_timeout(
    cmd: &mut Command,
    timeout: std::time::Duration,
) -> std::io::Result<std::process::Output> {
    let mut child = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;

    // Drain stdout/stderr on dedicated reader threads *while* the child runs.
    //
    // Reading each pipe as soon as data arrives (rather than only after the
    // child exits) is what makes a pipe deadlock impossible: the OS pipes we
    // hand to the child are backed by a fixed-size kernel buffer (64 KiB on
    // Windows). A child that writes more than that — e.g. Forge's `jarsplitter`
    // processor printing its full asset listing, or a noisy Java probe — would
    // otherwise block forever on write() once the buffer fills, `try_wait()`
    // would keep returning `None`, and the command would hang until killed.
    // Reading the pipes concurrently with `try_wait()` lets the child always
    // make progress, so it actually reaches its exit.
    let stdout_handle = spawn_reader(child.stdout.take());
    let stderr_handle = spawn_reader(child.stderr.take());

    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        format!("command timed out after {}s", timeout.as_secs()),
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(e) => break Err(e),
        }
    }?;

    // Join the reader threads to collect whatever output was produced. A
    // reader thread only unblocks once its pipe reaches EOF, which happens
    // once the (already-exited) child has been reaped and all handles to the
    // write end are dropped.
    let stdout = stdout_handle.join().unwrap_or_default();
    let stderr = stderr_handle.join().unwrap_or_default();

    Ok(std::process::Output {
        status,
        stdout,
        stderr,
    })
}

/// Spawn a background thread that reads a piped stream to EOF and returns the
/// captured bytes. Returns a joined handle; the handle yields an empty `Vec`
/// if the thread panicked or if `stream` was already taken.
fn spawn_reader<R: std::io::Read + Send + 'static>(
    stream: Option<R>,
) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut reader = match stream {
            Some(s) => s,
            None => return buf,
        };
        let _ = reader.read_to_end(&mut buf);
        buf
    })
}

/// Detected Java installation
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct JavaInstallation {
    pub path: PathBuf,
    pub version: String,
    pub major_version: u32,
    pub is_64bit: bool,
    pub vendor: String,
    /// Architecture of THIS JVM, derived from its own `os.arch` property when
    /// the run could be probed. Not the launcher's or the OS's architecture —
    /// a 32-bit launcher can run an arm64 JVM and vice versa. Probed per
    /// executable; defaults to `Unknown` for records without a probed arch.
    #[serde(default)]
    pub arch: JavaArch,
}

/// Architecture reported by a Java executable itself (`os.arch`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JavaArch {
    X86_64,
    X86,
    Arm64,
    #[default]
    Unknown,
}

/// Map a JVM `os.arch` property to [`JavaArch`]. Values are taken from real
/// JVMs: HotSpot reports `amd64` on Windows x64 (some builds `x86_64`),
/// `aarch64` on ARM64 and `x86`/`i386`/`i686` on 32-bit x86, while GraalVM and
/// other OpenJDK derivatives keep the same property names.
pub(crate) fn normalize_arch(raw: &str) -> JavaArch {
    match raw.trim().to_ascii_lowercase().as_str() {
        "amd64" | "x86_64" | "x86-64" | "x64" => JavaArch::X86_64,
        "i386" | "i686" | "x86" => JavaArch::X86,
        "aarch64" | "arm64" => JavaArch::Arm64,
        _ => JavaArch::Unknown,
    }
}

/// Machine-readable properties extracted from
/// `java -XshowSettings:properties -version`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProbeData {
    /// `java.version` (or the `version "…"` header line as a fallback).
    pub version: String,
    /// `java.vendor` as reported (may be an empty string when the property is
    /// missing; the caller then falls back to the text heuristic).
    pub vendor: String,
    /// `java.vm.name` (may be empty when missing).
    pub vm_name: String,
    /// `os.arch` as reported by the JVM itself.
    pub os_arch: String,
    /// `sun.arch.data.model` (32/64), absent in some non-standard JVMs.
    pub data_model: Option<u32>,
}

/// Split one probe line "key = value" into its parts. Property lines are
/// indented and separated from the value by " = "; anything without a key and
/// a value on both sides of the first "=" is not a property line.
fn split_property_line(line: &str) -> Option<(&str, &str)> {
    let trimmed = line.trim();
    let eq = trimmed.find('=')?;
    let key = trimmed[..eq].trim();
    let value = trimmed[eq + 1..].trim();
    if key.is_empty() || value.is_empty() {
        return None;
    }
    Some((key, value))
}

/// Parse the structured output of `java -XshowSettings:properties -version`.
///
/// Pure: no installed JDK is consulted. Returns `None` when the output does
/// not carry the properties we rely on (unsupported flag, corrupted output,
/// truncated capture) — the caller then falls back to the legacy text parser
/// instead of trusting invented defaults. `java.version` and `os.arch` are
/// required; `java.vendor`, `java.vm.name` and `sun.arch.data.model` are
/// optional because some non-standard JVMs omit them.
pub(crate) fn parse_probe_output(output: &str) -> Option<ProbeData> {
    let mut version = None;
    let mut vendor = None;
    let mut vm_name = None;
    let mut os_arch = None;
    let mut data_model = None;

    for line in output.lines() {
        let Some((key, value)) = split_property_line(line) else {
            continue;
        };
        match key {
            "java.version" => version = version.or_else(|| Some(value.to_string())),
            "java.vendor" => vendor = vendor.or_else(|| Some(value.to_string())),
            "java.vm.name" => vm_name = vm_name.or_else(|| Some(value.to_string())),
            "os.arch" => os_arch = os_arch.or_else(|| Some(value.to_string())),
            "sun.arch.data.model" => {
                data_model = data_model.or_else(|| value.parse::<u32>().ok());
            }
            _ => {}
        }
    }

    // `java.version` may be missing while the -version header line is intact
    // (e.g. a JVM that skips the property but still prints the banner).
    let version = version.or_else(|| parse_java_version(output));
    Some(ProbeData {
        version: version?,
        vendor: vendor.unwrap_or_default(),
        vm_name: vm_name.unwrap_or_default(),
        os_arch: os_arch?,
        data_model,
    })
}

/// Detect all Java installations on the system (Windows)
pub fn detect_java_installations() -> Vec<JavaInstallation> {
    let mut installations = Vec::new();
    let mut checked_paths: Vec<PathBuf> = Vec::new();

    // Check common Java locations on Windows
    let program_files =
        std::env::var("ProgramFiles").unwrap_or_else(|_| "C:\\Program Files".into());
    let program_files_x86 =
        std::env::var("ProgramFiles(x86)").unwrap_or_else(|_| "C:\\Program Files (x86)".into());
    let local_app_data = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| String::new());

    let search_dirs = vec![
        format!("{}\\Java", program_files),
        format!("{}\\Java", program_files_x86),
        format!("{}\\Eclipse Adoptium", program_files),
        format!("{}\\AdoptOpenJDK", program_files),
        format!("{}\\Zulu", program_files),
        format!("{}\\Microsoft\\jdk", program_files),
        format!("{}\\BellSoft\\LibericaJDK", program_files),
        format!("{}\\Amazon Corretto", program_files),
        format!(
            "{}\\Packages\\Microsoft.4297127D64EC6_8wekyb3d8bbwe\\LocalCache\\Local\\runtime",
            local_app_data
        ),
    ];

    for dir in &search_dirs {
        let dir_path = PathBuf::from(dir);
        if dir_path.exists() {
            if let Ok(entries) = std::fs::read_dir(&dir_path) {
                for entry in entries.flatten() {
                    let java_exe = entry.path().join("bin").join("java.exe");
                    let javaw_exe = entry.path().join("bin").join("javaw.exe");
                    let actual = if java_exe.exists() {
                        java_exe
                    } else if javaw_exe.exists() {
                        javaw_exe
                    } else {
                        continue;
                    };

                    if !checked_paths.contains(&actual) {
                        if let Some(install) = probe_java_by_path(&actual) {
                            checked_paths.push(actual);
                            installations.push(install);
                        }
                    }
                }
            }
        }
    }

    // Check JAVA_HOME
    if let Ok(java_home) = std::env::var("JAVA_HOME") {
        let java_exe = PathBuf::from(&java_home).join("bin").join("java.exe");
        if java_exe.exists() && !checked_paths.contains(&java_exe) {
            if let Some(install) = probe_java_by_path(&java_exe) {
                checked_paths.push(java_exe);
                installations.push(install);
            }
        }
    }

    // Check PATH
    if let Ok(path) = std::env::var("PATH") {
        for dir in path.split(';') {
            let java_exe = PathBuf::from(dir).join("java.exe");
            if java_exe.exists() && !checked_paths.contains(&java_exe) {
                if let Some(install) = probe_java_by_path(&java_exe) {
                    checked_paths.push(java_exe);
                    installations.push(install);
                }
            }
        }
    }

    // Sort by major version descending
    installations.sort_by_key(|b| std::cmp::Reverse(b.major_version));
    installations
}

/// How long a Java probe may run before the child is killed. A stuck
/// java.exe (mid-update, broken install, locked by antivirus) must not hang
/// the launcher; kept as a constant so tests can use shorter timeouts.
pub(crate) const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// Probe a Java executable to get version info (default 20 s timeout).
/// Skips files smaller than 10KB (MSI stubs / broken installations).
pub fn probe_java_by_path(path: &Path) -> Option<JavaInstallation> {
    probe_java_by_path_with_timeout(path, PROBE_TIMEOUT)
}

/// Probe with a caller-provided timeout so tests can exercise timeouts
/// without waiting on real JVM startup.
pub(crate) fn probe_java_by_path_with_timeout(
    path: &Path,
    timeout: std::time::Duration,
) -> Option<JavaInstallation> {
    probe_java_with_runner(path, timeout, run_command_with_timeout)
}

/// Probe using an injectable process runner. Production always passes
/// [`run_command_with_timeout`]; tests replace it to describe a fake JVM's
/// output without requiring a real JRE on the machine.
fn probe_java_with_runner(
    path: &Path,
    timeout: std::time::Duration,
    run: impl Fn(&mut Command, std::time::Duration) -> std::io::Result<std::process::Output>,
) -> Option<JavaInstallation> {
    // Skip tiny executables — real java.exe is 30KB+, MSI stubs are ~10-50KB
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() < 10_000 {
        tracing::debug!(target: "launcher", "Skipping small java.exe ({} bytes): {:?}", meta.len(), path);
        return None;
    }

    // Step 1: machine-readable probe. `-version` is needed to make the JVM
    // actually start; `-XshowSettings:properties` then dumps the property
    // report. HotSpot prints everything to stderr, some other JVMs to stdout
    // — both streams are captured, stderr first.
    let mut cmd = Command::new(path);
    cmd.arg("-XshowSettings:properties").arg("-version");
    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);

    if let Ok(output) = run(&mut cmd, timeout) {
        let probe_text = probe_output_text(&output);
        if output.status.success() {
            if let Some(data) = parse_probe_output(&probe_text) {
                tracing::debug!(
                    target: "launcher",
                    "Java probe at {:?}: version={}, vendor='{}', vm='{}', os.arch='{}', bits={:?}",
                    path,
                    data.version,
                    data.vendor,
                    data.vm_name,
                    data.os_arch,
                    data.data_model
                );
                return Some(installation_from_probe(path, &data, &probe_text));
            }
        }
        tracing::debug!(
            target: "launcher",
            "Structured Java probe unusable at {:?} (status={}): fallback to text parser",
            path,
            output.status
        );
    }

    // Step 2: fallback to the legacy `java -version` text parser.
    probe_java_text_parser_with_runner(path, timeout, &run)
}

/// Combine the capture of a probe run: properties (and `-version`) usually go
/// to stderr, so it wins; stdout carries the same content on other JVMs.
fn probe_output_text(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    format!("{}\n{}", stderr, stdout)
}

/// Build an installation from structured probe properties.
fn installation_from_probe(path: &Path, data: &ProbeData, probe_text: &str) -> JavaInstallation {
    let arch = normalize_arch(&data.os_arch);
    // Bits: trust the machine-readable property; otherwise infer from the
    // architecture the JVM itself reported.
    let is_64bit = match data.data_model {
        Some(64) => true,
        Some(32) => false,
        _ => match arch {
            JavaArch::X86_64 | JavaArch::Arm64 => true,
            JavaArch::X86 => false,
            JavaArch::Unknown => default_is_64bit_from_text(probe_text),
        },
    };
    // Keep the same friendly vendor labels the UI has always seen. The raw
    // property (e.g. "Azul Systems, Inc.", "GraalVM Community") plus the VM
    // name carry recognizable substrings, so the text heuristic still finds
    // them without a vendor-specific mapping table.
    let vendor = friendly_vendor(data, probe_text);

    JavaInstallation {
        path: path.to_path_buf(),
        version: data.version.clone(),
        major_version: parse_major_version(&data.version),
        is_64bit,
        vendor,
        arch,
    }
}

/// Friendly vendor label for the UI: prefer the structured `java.vendor` and
/// `java.vm.name` properties; when the property is missing, search the
/// whole probe text (the legacy behaviour).
fn friendly_vendor(data: &ProbeData, probe_text: &str) -> String {
    if data.vendor.is_empty() {
        parse_vendor(probe_text)
    } else {
        parse_vendor(&format!("{} {}", data.vendor, data.vm_name))
    }
}

/// Legacy fallback: run `java -version` and parse its text output. Kept
/// intact until the structured probe is confirmed compatible everywhere.
fn probe_java_text_parser_with_runner(
    path: &Path,
    timeout: std::time::Duration,
    run: &impl Fn(&mut Command, std::time::Duration) -> std::io::Result<std::process::Output>,
) -> Option<JavaInstallation> {
    let mut cmd = Command::new(path);
    cmd.arg("-version");
    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let output = match run(&mut cmd, timeout) {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!(target: "launcher", "Failed to run java -version at {:?}: {}", path, e);
            return None;
        }
    };

    if !output.status.success() {
        tracing::warn!(target: "launcher", "java -version failed at {:?}: status={}, stderr={}, stdout={}",
            path, output.status,
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout));
        return None;
    }

    // Java prints version to stderr
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let version_output = if stderr.contains("version") {
        stderr.to_string()
    } else {
        stdout.to_string()
    };

    let version = parse_java_version(&version_output).unwrap_or_default();
    if version.is_empty() {
        tracing::warn!(target: "launcher", "Could not parse java version from {:?}: stderr={}", path, stderr);
        return None;
    }
    let major = parse_major_version(&version);
    let is_64bit = default_is_64bit_from_text(&version_output);
    let vendor = parse_vendor(&version_output);

    Some(JavaInstallation {
        path: path.to_path_buf(),
        version,
        major_version: major,
        is_64bit,
        vendor,
        arch: JavaArch::Unknown,
    })
}

/// Text heuristic kept for the legacy parser and for the rare case where the
/// JVM reported neither `sun.arch.data.model` nor a recognizable `os.arch`.
fn default_is_64bit_from_text(output: &str) -> bool {
    output.contains("64-Bit") || output.contains("64-bit")
}

/// Parse Java version string from `java -version` output
fn parse_java_version(output: &str) -> Option<String> {
    // Matches patterns like: "21.0.1" or "1.8.0_382"
    for line in output.lines() {
        if line.contains("version") {
            let start = line.find('"')?;
            let end = line[start + 1..].find('"')?;
            return Some(line[start + 1..start + 1 + end].to_string());
        }
    }
    None
}

/// Parse major version number
fn parse_major_version(version: &str) -> u32 {
    // Take the leading digit run of each segment, so suffixed versions like
    // "25-ea+12" still resolve their numbers ("25"), while the legacy format
    // "1.8.0_382" keeps mapping 1.8 → 8.
    let leading_number = |segment: &str| -> Option<u32> {
        let digits_end = segment
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(segment.len());
        if digits_end == 0 {
            return None;
        }
        segment[..digits_end].parse::<u32>().ok()
    };
    let mut parts = version.split('.');
    let first = leading_number(parts.next().unwrap_or("")).unwrap_or(0);
    if first == 1 {
        // Old format: 1.8.0 → major is 8
        return leading_number(parts.next().unwrap_or("")).unwrap_or(0);
    }
    first
}

/// Parse vendor from version output
fn parse_vendor(output: &str) -> String {
    if output.contains("Eclipse Adoptium") || output.contains("Temurin") {
        "Eclipse Adoptium".to_string()
    } else if output.contains("GraalVM") {
        "GraalVM".to_string()
    } else if output.contains("Zulu") || output.contains("Azul") {
        "Azul Zulu".to_string()
    } else if output.contains("Corretto") {
        "Amazon Corretto".to_string()
    } else if output.contains("Microsoft") {
        "Microsoft".to_string()
    } else if output.contains("OpenJDK") {
        "OpenJDK".to_string()
    } else if output.contains("Oracle") || output.contains("Java(TM)") {
        "Oracle".to_string()
    } else {
        "Unknown".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::time::Duration;

    /// Child-mode marker: when set, the test binary acts as a helper that
    /// writes a large volume of output to BOTH stdout and stderr (well past
    /// the ~64 KiB OS pipe buffer) and exits successfully. This reproduces the
    /// scenario that would deadlock a naive "drain after exit" implementation.
    const HELPER_ENV: &str = "VOIDLAUNCHER_RUN_CMD_HELPER";

    fn helper_writes_big_output() -> bool {
        std::env::var(HELPER_ENV).as_deref() == Ok("big-output")
    }

    #[test]
    fn run_command_with_timeout_drains_large_stdout_and_stderr() {
        if helper_writes_big_output() {
            // Child mode: emit enough data on both pipes to fill and overflow
            // the OS pipe buffer, then exit 0.
            let chunk = vec![b'x'; 8192];
            let mut out = std::io::stdout();
            let mut err = std::io::stderr();
            for _ in 0..256 {
                let _ = out.write_all(&chunk);
                let _ = err.write_all(&chunk);
            }
            let _ = out.flush();
            let _ = err.flush();
            return;
        }

        // Parent mode: spawn our own binary in child mode through
        // run_command_with_timeout and assert it completes normally. Run only
        // this test in the child: executing the whole suite would make the
        // helper's exit code depend on unrelated (and possibly flaky) tests.
        let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
        cmd.env(HELPER_ENV, "big-output");
        cmd.arg("java::tests::run_command_with_timeout_drains_large_stdout_and_stderr");
        cmd.arg("--exact");

        let output = run_command_with_timeout(&mut cmd, Duration::from_secs(30))
            .expect("command should complete without hanging");

        assert!(output.status.success(), "helper should exit 0");
        // The helper emits 256 chunks of 8192 bytes = 2 MiB per stream, well
        // past the ~64 KiB OS pipe buffer. The captured output must be at
        // least that full volume (the child also appends a bit of test-harness
        // output), which proves the pipes were drained while the child ran
        // instead of deadlocking and being killed.
        let expected = 256 * 8192;
        assert!(
            output.stdout.len() >= expected,
            "stdout should be fully drained (got {}, want at least {})",
            output.stdout.len(),
            expected
        );
        assert!(
            output.stderr.len() >= expected,
            "stderr should be fully drained (got {}, want at least {})",
            output.stderr.len(),
            expected
        );
    }

    /// The timeout path must still work: a child that never exits is killed
    /// and reported as TimedOut, not left to hang the test forever.
    #[test]
    fn run_command_with_timeout_kills_on_timeout() {
        if helper_writes_big_output() {
            // not used by this test
            return;
        }
        let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
        cmd.env(HELPER_ENV, "big-output");
        // A 30s child with a 50ms timeout must be reaped and surface TimedOut.
        let start = std::time::Instant::now();
        let res = run_command_with_timeout(&mut cmd, Duration::from_millis(50));
        let elapsed = start.elapsed();
        let err = res.expect_err("should time out");
        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
        assert!(
            elapsed < Duration::from_secs(10),
            "timeout kill took too long: {:?}",
            elapsed
        );
    }

    // -----------------------------
    // M3.1: structured JVM probing
    // -----------------------------

    /// Realistic Temurin 21 stderr for `-XshowSettings:properties -version`.
    const TEMURIN_21: &str = "\
openjdk version \"21.0.3\" 2024-04-16
OpenJDK Runtime Environment Temurin-21.0.3+9 (build 21.0.3+9)
OpenJDK 64-Bit Server VM Temurin-21.0.3+9 (build 21.0.3+9, mixed mode, sharing)

 Property settings:
    file.encoding = UTF-8
    java.runtime.version = 21.0.3+9-LTS
    java.version = 21.0.3
    java.vendor = Eclipse Adoptium
    java.vm.name = OpenJDK 64-Bit Server VM
    os.arch = amd64
    sun.arch.data.model = 64
";

    /// Temurin 17 keeps the same shape with a different version.
    const TEMURIN_17: &str = "\
openjdk version \"17.0.11\" 2024-04-16
OpenJDK Runtime Environment Temurin-17.0.11+9 (build 17.0.11+9)
OpenJDK 64-Bit Server VM Temurin-17.0.11+9 (build 17.0.11+9, mixed mode, sharing)

 Property settings:
    java.version = 17.0.11
    java.vendor = Eclipse Adoptium
    java.vm.name = OpenJDK 64-Bit Server VM
    os.arch = amd64
    sun.arch.data.model = 64
";

    /// Temurin 8 uses the legacy "1.8.0_xxx" format and an indented property
    /// block with different surroundings.
    const TEMURIN_8: &str = "\
openjdk version \"1.8.0_412\" 2024-07-18
OpenJDK Runtime Environment Temurin-1.8.0_412-b08 (build 1.8.0_412-b08)
OpenJDK 64-Bit Server VM (build 25.412-b08, mixed mode)

 Property settings:
    java.version = 1.8.0_412
    java.vendor = Temurin
    java.vm.name = OpenJDK 64-Bit Server VM
    os.arch = amd64
    sun.arch.data.model = 64
";

    /// Early-access builds carry suffixes OS toolchains typically mangle.
    const TEMURIN_25_EA: &str = "\
openjdk version \"25-ea+12\" 2025-03-19
OpenJDK Runtime Environment Temurin-25.0.0-ea+12 (build 25-ea+12)
OpenJDK 64-Bit Server VM Temurin-25.0.0-ea+12 (build 25-ea+12, mixed mode, sharing)

 Property settings:
    java.version = 25-ea+12
    java.vendor = Eclipse Adoptium
    java.vm.name = OpenJDK 64-Bit Server VM
    os.arch = amd64
    sun.arch.data.model = 64
";

    /// Oracle 8: vendor string is "Oracle Corporation".
    const ORACLE_8: &str = "\
java version \"1.8.0_402\" 2024-01-16
Java(TM) SE Runtime Environment (build 1.8.0_402-b11)
Java HotSpot(TM) 64-Bit Server VM (build 25.402-b11, mixed mode)

Property settings:
    java.version = 1.8.0_402
    java.vendor = Oracle Corporation
    java.vm.name = Java HotSpot(TM) 64-Bit Server VM
    os.arch = amd64
    sun.arch.data.model = 64
";

    /// GraalVM: custom vendor string and VM name.
    const GRAALVM: &str = "\
openjdk version \"17.0.9\" 2023-10-17
OpenJDK Runtime Environment GraalVM 17.0.9+9.1 (build 17.0.9+9.1)
OpenJDK GraalVM CE 64-Bit Server VM GraalVM 17.0.9+9.1 (build 17.0.9+9.1, mixed mode, shared threads)

Property settings:
    java.version = 17.0.9
    java.vendor = GraalVM Community
    java.vm.name = OpenJDK GraalVM CE 64-Bit Server VM
    os.arch = amd64
    sun.arch.data.model = 64
";

    /// Zulu: the vendor property is the corporate name.
    const ZULU: &str = "\
openjdk version \"21.0.2\" 2024-01-16
OpenJDK Runtime Environment Zulu21.30+15-CA (build 21.0.2+13-LTS)
OpenJDK 64-Bit Server VM Zulu21.30+15-CA (build 21.0.2+13-LTS, mixed mode, sharing)

Property settings:
    java.version = 21.0.2
    java.vendor = Azul Systems, Inc.
    java.vm.name = OpenJDK 64-Bit Server VM
    os.arch = amd64
    sun.arch.data.model = 64
";

    #[test]
    fn parse_probe_output_extracts_realistic_vendor_outputs() {
        let fixtures = [
            (
                "Temurin 21",
                TEMURIN_21,
                "21.0.3",
                "Eclipse Adoptium",
                "amd64",
                Some(64u32),
                "Eclipse Adoptium",
            ),
            (
                "Temurin 17",
                TEMURIN_17,
                "17.0.11",
                "Eclipse Adoptium",
                "amd64",
                Some(64),
                "Eclipse Adoptium",
            ),
            (
                "Temurin 8",
                TEMURIN_8,
                "1.8.0_412",
                "Temurin",
                "amd64",
                Some(64),
                "Eclipse Adoptium",
            ),
            (
                "Temurin 25-ea",
                TEMURIN_25_EA,
                "25-ea+12",
                "Eclipse Adoptium",
                "amd64",
                Some(64),
                "Eclipse Adoptium",
            ),
            (
                "Oracle 8",
                ORACLE_8,
                "1.8.0_402",
                "Oracle Corporation",
                "amd64",
                Some(64),
                "Oracle",
            ),
            (
                "GraalVM",
                GRAALVM,
                "17.0.9",
                "GraalVM Community",
                "amd64",
                Some(64),
                "GraalVM",
            ),
            (
                "Zulu",
                ZULU,
                "21.0.2",
                "Azul Systems, Inc.",
                "amd64",
                Some(64),
                "Azul Zulu",
            ),
        ];
        for (name, output, version, vendor, os_arch, data_model, friendly) in fixtures {
            let data = parse_probe_output(output).expect(name);
            assert_eq!(data.version, version, "version: {name}");
            assert_eq!(data.vendor, vendor, "vendor: {name}");
            assert_eq!(data.os_arch, os_arch, "os.arch: {name}");
            assert_eq!(data.data_model, data_model, "data model: {name}");
            assert!(!data.vm_name.is_empty(), "vm.name must be captured: {name}");
            assert_eq!(
                friendly_vendor(&data, output),
                friendly,
                "friendly vendor: {name}"
            );
        }
    }

    #[test]
    fn parse_probe_output_reports_first_occurrence_and_ignores_other_lines() {
        let data = parse_probe_output(TEMURIN_21).expect("temurin 21 must parse");
        assert_eq!(data.version, "21.0.3");

        // The `java.version` property wins over anything found on the banner.
        let banner = "\
java version \"9.0.4\"
OpenJDK Runtime Environment (build 9.0.4+11)
 Property settings:
    java.version = 17.0.3
    os.arch = amd64
";
        let data = parse_probe_output(banner).expect("banner must parse");
        assert_eq!(data.version, "17.0.3");
    }

    #[test]
    fn parse_probe_output_version_falls_back_to_banner_line() {
        // A JVM that lists no java.version property still prints the banner.
        let output = "\
openjdk version \"11.0.20\" 2023-07-18
 Property settings:
    java.vendor = Some Build
    java.vm.name = MiniVM
    os.arch = x86_64
";
        let data = parse_probe_output(output).expect("banner fallback must parse");
        assert_eq!(data.version, "11.0.20");
        assert_eq!(data.vendor, "Some Build");
        assert_eq!(data.vm_name, "MiniVM");
        assert_eq!(normalize_arch(&data.os_arch), JavaArch::X86_64);
        assert_eq!(data.data_model, None, "missing data model stays absent");
    }

    #[test]
    fn parse_probe_output_rejects_damaged_and_incomplete_output() {
        let broken = [
            ("empty", ""),
            ("prose only", "openjdk version \"21.0.3\"\njava was here"),
            (
                "no os.arch",
                " Property settings:\n    java.version = 21.0.3\n",
            ),
            (
                "no java.version",
                " Property settings:\n    os.arch = amd64\n",
            ),
            ("equal sign only", "no properties have = here"),
            (
                "empty values",
                " Property settings:\n    java.version = \n    os.arch = amd64\n",
            ),
        ];
        for (name, output) in broken {
            assert!(parse_probe_output(output).is_none(), "must reject: {name}");
        }
    }

    #[test]
    fn normalize_arch_maps_real_jvm_values() {
        let cases = [
            ("amd64", JavaArch::X86_64),
            ("x86_64", JavaArch::X86_64),
            ("AMD64", JavaArch::X86_64),
            ("i386", JavaArch::X86),
            ("i686", JavaArch::X86),
            ("aarch64", JavaArch::Arm64),
            ("arm64", JavaArch::Arm64),
            ("riscv64", JavaArch::Unknown),
            ("", JavaArch::Unknown),
            ("  amd64  ", JavaArch::X86_64),
        ];
        for (raw, expected) in cases {
            assert_eq!(normalize_arch(raw), expected, "raw: '{raw}'");
        }
    }

    #[test]
    fn parse_major_version_handles_suffixes_and_legacy_format() {
        let cases = [
            ("21.0.3", 21u32),
            ("1.8.0_402", 8),
            ("17.0.11", 17),
            ("25-ea+12", 25),
            ("9", 9),
            ("garbage", 0),
        ];
        for (version, expected) in cases {
            assert_eq!(parse_major_version(version), expected, "version: {version}");
        }
    }

    // Fake Java protocol. These checks must not require a real JRE or a
    // network: instead of spawning a process, the probe runs against a fake
    // runner that reacts like a Java executable would.

    fn fake_status(code: i32) -> std::process::ExitStatus {
        #[cfg(windows)]
        {
            use std::os::windows::process::ExitStatusExt;
            // The Windows raw encoding stores the exit code directly.
            std::process::ExitStatus::from_raw(code as u32)
        }
        #[cfg(not(windows))]
        {
            use std::os::unix::process::ExitStatusExt;
            std::process::ExitStatus::from_raw(code)
        }
    }

    fn output_ok(stderr: &[u8]) -> std::io::Result<std::process::Output> {
        Ok(std::process::Output {
            status: fake_status(0),
            stdout: Vec::new(),
            stderr: stderr.to_vec(),
        })
    }

    /// Emulate the process runs of a Java executable. The structured call
    /// (`-XshowSettings:properties -version`) and the legacy fallback call
    /// (`-version`) resolve to the same output, mirroring a JVM whose report
    /// is identical for both invocations.
    fn fake_java_run(
        mode: &'static str,
    ) -> impl Fn(&mut Command, Duration) -> std::io::Result<std::process::Output> {
        move |_cmd, _timeout| {
            match mode {
                "structured-amd64" => output_ok(TEMURIN_21.as_bytes()),
                "structured-arm64" => {
                    // Same Temurin output, but this JVM reports `aarch64` —
                    // the only difference from the amd64 JVM's stderr, so the
                    // architecture provably comes from the JVM's own report.
                    let arm64 = TEMURIN_21
                        .lines()
                        .map(|line| {
                            if line.contains("os.arch") {
                                "    os.arch = aarch64"
                            } else {
                                line
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    output_ok(format!("{}\n", arm64).as_bytes())
                }
                "legacy-only" => output_ok(
                    b"openjdk version \"17.0.9\" 2023-10-17\n\
                      OpenJDK Runtime Environment (build 17.0.9+9)\n\
                      OpenJDK 64-Bit Server VM (build 17.0.9+9, mixed mode)\n",
                ),
                "exit-1" => Ok(std::process::Output {
                    status: fake_status(1),
                    stdout: Vec::new(),
                    stderr: b"Unrecognized option: -XshowSettings:properties".to_vec(),
                }),
                "spawn-error" => Err(std::io::Error::other("access denied")),
                "timed-out" => Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "command timed out after 0s",
                )),
                _ => Err(std::io::Error::other("unexpected fake mode")),
            }
        }
    }

    /// A side-effect-free stand-in path that passes the size floor; the fake
    /// runner means no process is actually spawned for it.
    fn fake_java_file(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "voidlauncher-fake-java-{}-{}.exe",
            std::process::id(),
            label
        ));
        std::fs::write(&path, vec![0u8; 12_000]).expect("write fake executable file");
        path
    }

    #[test]
    fn probe_java_by_path_reports_jvm_own_arch_and_falls_back_safely() {
        let exe = fake_java_file("m3-probe");
        let timeout = Duration::from_secs(20);
        let probe = |mode: &'static str| probe_java_with_runner(&exe, timeout, fake_java_run(mode));

        // 1. Structured probe: everything comes from the JVM's properties.
        let inst = probe("structured-amd64").expect("amd64 probe must succeed");
        assert_eq!(inst.path, exe);
        assert_eq!(inst.version, "21.0.3");
        assert_eq!(inst.major_version, 21);
        assert_eq!(inst.vendor, "Eclipse Adoptium");
        assert_eq!(inst.arch, JavaArch::X86_64);
        assert!(inst.is_64bit);

        // 2. The same probed path with an arm64 JVM report. The probe side is
        //    identical in both runs, so only the JVM's own output explains
        //    the different architecture.
        let inst = probe("structured-arm64").expect("arm64 probe must succeed");
        assert_eq!(inst.version, "21.0.3");
        assert_eq!(inst.arch, JavaArch::Arm64);
        assert!(inst.is_64bit);

        // 3. Fallback: no structured properties -> legacy text parser. The
        //    arch stays Unknown (no false arch from the probe side).
        let inst = probe("legacy-only").expect("legacy fallback must succeed");
        assert_eq!(inst.version, "17.0.9");
        assert_eq!(inst.major_version, 17);
        assert_eq!(inst.vendor, "OpenJDK");
        assert_eq!(inst.arch, JavaArch::Unknown);
        assert!(inst.is_64bit, "text heuristic keeps bitness");

        // 4. Executable that exits without usable output.
        let inst = probe("exit-1");
        assert!(inst.is_none(), "failing executable must not be probed");

        // 5. A spawn that fails outright (broken installation).
        let inst = probe("spawn-error");
        assert!(inst.is_none(), "failed spawn must not be probed");

        // 6. A run that hits the timeout must surface as None, promptly.
        let start = std::time::Instant::now();
        let inst = probe("timed-out");
        let elapsed = start.elapsed();
        assert!(inst.is_none(), "timed-out probe must return None");
        assert!(
            elapsed < Duration::from_secs(10),
            "probe must be bounded by its timeout, took {:?}",
            elapsed
        );

        std::fs::remove_file(&exe).expect("clean up fake executable file");
    }
}

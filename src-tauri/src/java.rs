use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Command;

/// Detected Java installation
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct JavaInstallation {
    pub path: PathBuf,
    pub version: String,
    pub major_version: u32,
    pub is_64bit: bool,
    pub vendor: String,
}

/// Detect all Java installations on the system (Windows)
pub fn detect_java_installations() -> Vec<JavaInstallation> {
    let mut installations = Vec::new();
    let mut checked_paths: Vec<PathBuf> = Vec::new();

    // Check common Java locations on Windows
    let program_files = std::env::var("ProgramFiles").unwrap_or_else(|_| "C:\\Program Files".into());
    let program_files_x86 =
        std::env::var("ProgramFiles(x86)").unwrap_or_else(|_| "C:\\Program Files (x86)".into());
    let local_app_data =
        std::env::var("LOCALAPPDATA").unwrap_or_else(|_| String::new());

    let search_dirs = vec![
        format!("{}\\Java", program_files),
        format!("{}\\Java", program_files_x86),
        format!("{}\\Eclipse Adoptium", program_files),
        format!("{}\\AdoptOpenJDK", program_files),
        format!("{}\\Zulu", program_files),
        format!("{}\\Microsoft\\jdk", program_files),
        format!("{}\\BellSoft\\LibericaJDK", program_files),
        format!("{}\\Amazon Corretto", program_files),
        format!("{}\\Packages\\Microsoft.4297127D64EC6_8wekyb3d8bbwe\\LocalCache\\Local\\runtime", local_app_data),
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
                        if let Some(install) = probe_java(&actual) {
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
            if let Some(install) = probe_java(&java_exe) {
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
                if let Some(install) = probe_java(&java_exe) {
                    checked_paths.push(java_exe);
                    installations.push(install);
                }
            }
        }
    }

    // Sort by major version descending
    installations.sort_by(|a, b| b.major_version.cmp(&a.major_version));
    installations
}

/// Probe a Java executable to get version info
fn probe_java(path: &PathBuf) -> Option<JavaInstallation> {
    let output = Command::new(path)
        .arg("-version")
        .output()
        .ok()?;

    // Java prints version to stderr
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let version_output = if stderr.contains("version") {
        stderr.to_string()
    } else {
        stdout.to_string()
    };

    let version = parse_java_version(&version_output)?;
    let major = parse_major_version(&version);
    let is_64bit = version_output.contains("64-Bit");
    let vendor = parse_vendor(&version_output);

    Some(JavaInstallation {
        path: path.clone(),
        version,
        major_version: major,
        is_64bit,
        vendor,
    })
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
    let parts: Vec<&str> = version.split('.').collect();
    if let Some(first) = parts.first() {
        let num = first.parse::<u32>().unwrap_or(0);
        if num == 1 && parts.len() > 1 {
            // Old format: 1.8.0 → major is 8
            return parts[1].parse::<u32>().unwrap_or(0);
        }
        return num;
    }
    0
}

/// Parse vendor from version output
fn parse_vendor(output: &str) -> String {
    if output.contains("Eclipse Adoptium") || output.contains("Temurin") {
        "Eclipse Adoptium".to_string()
    } else if output.contains("GraalVM") {
        "GraalVM".to_string()
    } else if output.contains("Zulu") {
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

/// Get recommended Java for a Minecraft version
pub fn get_recommended_java(
    mc_java_version: Option<u32>,
    installations: &[JavaInstallation],
) -> Option<JavaInstallation> {
    let required_major = mc_java_version.unwrap_or(21);

    // Prefer exact match, then closest higher version
    installations
        .iter()
        .filter(|j| j.is_64bit && j.major_version >= required_major)
        .min_by_key(|j| j.major_version - required_major)
        .cloned()
}

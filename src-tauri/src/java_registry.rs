//! Discovery of installed Java runtimes through the Windows registry.
//!
//! MSI installers of Java vendors register their runtimes under
//! `HKLM\Software\...`, which catches installations that no filesystem scan
//! covers (custom install locations, per-user installs, private company
//! images). Vendor layouts differ — and are NOT uniform:
//!
//! - Oracle / JavaSoft keys (`JDK`, `JRE`, the Java 8 `Java Runtime
//!   Environment`, and the plug-in branch) store the installation directory
//!   in a `JavaHome` REG_SZ value; the Java 8 JRE additionally stores the
//!   `jvm.dll` path in a separate `RuntimeLib` value.
//! - Eclipse Adoptium and Microsoft JDK share the msi- bridge layout: the
//!   installation directory lives in a `Path` REG_SZ value on the
//!   `<version>\hotspot\MSI` key, where `JavaHome` is a DWORD *flag* —
//!   reading it as a string must fail, not invent a path.
//! - Azul Zulu, BellSoft Liberica and IBM Semeru keep per-version keys with
//!   the home directory under one of the value names accepted below.
//!
//! The registry only provides *candidate paths*. Version, vendor, bitness
//! and architecture are never trusted from registry data — every candidate
//! executable is actually probed (also done in M3.1) before it becomes an
//! installation. Views are read in both the 64-bit and the 32-bit variants:
//! 32-bit installers register under `HKLM\Software\Wow6432Node\...`, which
//! Windows surfaces transparently through `KEY_WOW64_32KEY`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// HKLM registry view a read is served from.
///
/// On 64-bit Windows a 32-bit process normally sees the `Wow6432Node`
/// reflection, so reading *both* views explicitly is the only way to see
/// 32-bit-registered JDKs from our 64-bit build and vice versa.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum RegistryView {
    /// Native 64-bit view (`HKLM\Software\...`).
    X64,
    /// 32-bit compatibility view (`HKLM\Software\Wow6432Node\...`).
    X32,
}

/// Read-only, minimal access to HKLM.
///
/// Windows uses the real registry; tests supply a fake. Reads of a missing or
/// inaccessible key must not fail the scan — they simply return nothing, so
/// one broken vendor key can never hide the others.
pub(crate) trait RegistryReader {
    /// Names of the direct subkeys of the HKLM key at `path` (order is not
    /// guaranteed; callers must not depend on it).
    fn subkeys(&self, path: &str, view: RegistryView) -> Vec<String>;
    /// A string (REG_SZ / expandable) value on the HKLM key at `path`. A DWORD
    /// value under the same name (the Adoptium `JavaHome` flag) must read as
    /// absent, never as a made-up path.
    fn value(&self, path: &str, name: &str, view: RegistryView) -> Option<String>;
}

/// Vendor roots that register installed JDKs/JREs (HKLM, relative to
/// `Software`).
///
/// The seven vendor branches specified for M3.2, plus `SOFTWARE\JavaSoft\
/// Java Runtime Environment`: that is where real Java 8 JREs actually
/// register (verified on a live machine — `SOFTWARE\JavaSoft\JRE` does not
/// exist for Java 8), and it carries a `JavaHome`/`RuntimeLib` pair the
/// launcher would otherwise miss.
pub(crate) const REGISTRY_ROOTS: &[&str] = &[
    r"SOFTWARE\JavaSoft\JDK",
    r"SOFTWARE\JavaSoft\JRE",
    r"SOFTWARE\JavaSoft\Java Runtime Environment",
    r"SOFTWARE\Eclipse Adoptium\JDK",
    r"SOFTWARE\Microsoft\JDK",
    r"SOFTWARE\Azul Systems\Zulu",
    r"SOFTWARE\BellSoft\LibericaJDK",
    r"SOFTWARE\IBM\Semeru",
];

/// Deepest nesting walked below each root. Adoptium/Microsoft reach their
/// values at `<version>\hotspot\MSI` (3 levels); the cap keeps a pathological
/// tree from branching in deep subkeys.
const MAX_DEPTH: usize = 5;

/// String value names that may carry the installation directory. Different
/// vendors (and different installer generations of the same vendor) pick
/// different names, so the scan accepts them all — wrong-typed or empty
/// values fail without consequences because the candidate is validated later.
const HOME_VALUE_NAMES: &[&str] = &[
    "JavaHome",
    "Path",
    "InstallationPath",
    "HomePath",
    "JAVA_HOME",
    "INSTALLDIR",
];

/// value name under which Java 8 JREs record the full path of their JVM
/// library (`...\bin\server\jvm.dll`), from which `java.exe` is derived.
const RUNTIME_LIB_NAME: &str = "RuntimeLib";

/// Candidate java.exe paths harvested from HKLM registry records.
///
/// Pure over the supplied reader: no real registry access. Duplicates (the
/// same executable registered twice, e.g. as `1.8` and `1.8.0_xxx`) collapse
/// into one candidate in a deterministic first-seen order.
pub(crate) fn registry_java_candidates(reader: &dyn RegistryReader) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for root in REGISTRY_ROOTS {
        for view in [RegistryView::X64, RegistryView::X32] {
            walk_root(reader, root, view, 0, &mut seen, &mut out);
        }
    }
    out
}

/// Depth-limited recursive walk of one HKLM subtree in one view. Reads of
/// individual keys may fail (locked, corrupted, deleted mid-scan); those keys
/// contribute nothing while siblings are still scanned.
fn walk_root(
    reader: &dyn RegistryReader,
    key_path: &str,
    view: RegistryView,
    depth: usize,
    seen: &mut HashSet<String>,
    out: &mut Vec<PathBuf>,
) {
    if depth > MAX_DEPTH {
        tracing::debug!(target: "launcher", "Registry Java scan: subtree too deep at {key_path}, stopping here");
        return;
    }
    for exe in key_candidates(reader, key_path, view) {
        // Windows paths are case-insensitive; deduplicate without the FS by
        // comparing lowercased executable paths.
        let id = exe.to_string_lossy().to_ascii_lowercase();
        if seen.insert(id) {
            out.push(exe);
        }
    }
    for sub in reader.subkeys(key_path, view) {
        if sub.is_empty() {
            continue;
        }
        walk_root(
            reader,
            &format!("{key_path}\\{sub}"),
            view,
            depth + 1,
            seen,
            out,
        );
    }
}

/// Extract java.exe candidates from the string values of ONE key.
///
/// A `JavaHome`-like value usually holds the installation directory
/// (`<home>\bin\java.exe`), but some records store the `bin` directory
/// directly; both spellings are accepted. `RuntimeLib` holds a `jvm.dll`
/// path, whose two ancestors are the `bin` directory.
fn key_candidates(reader: &dyn RegistryReader, key_path: &str, view: RegistryView) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for name in HOME_VALUE_NAMES {
        // A DWORD `JavaHome`/`Path` flag (Adoptium/Microsoft MSI keys) reads
        // as None here: only string values can become homes.
        let Some(home) = reader.value(key_path, name, view) else {
            continue;
        };
        let home = home.trim().trim_matches('\\');
        if home.is_empty() {
            continue;
        }
        let home = PathBuf::from(home);
        if !home.is_absolute() {
            continue;
        }
        let exe = if home
            .file_name()
            .is_some_and(|f| f.eq_ignore_ascii_case("bin"))
        {
            home.join("java.exe")
        } else {
            home.join("bin").join("java.exe")
        };
        out.push(exe);
    }
    if let Some(lib) = reader.value(key_path, RUNTIME_LIB_NAME, view) {
        let lib = PathBuf::from(lib.trim());
        if lib
            .file_name()
            .is_some_and(|f| f.eq_ignore_ascii_case("jvm.dll"))
        {
            // `...\bin\server\jvm.dll` (or `...\bin\client\jvm.dll`) — two
            // levels up is the `bin` directory that holds java.exe.
            if let Some(bin) = lib.parent().and_then(Path::parent) {
                out.push(bin.join("java.exe"));
            }
        }
    }
    out
}

/// Registry candidates from the machine's real registry; empty on other
/// platforms.
pub(crate) fn registry_candidates_from_os() -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        registry_java_candidates(&WindowsRegistryReader)
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

/// Read-only `RegistryReader` over the real Windows registry (HKLM).
#[cfg(windows)]
pub(crate) struct WindowsRegistryReader;

#[cfg(windows)]
impl RegistryReader for WindowsRegistryReader {
    fn subkeys(&self, path: &str, view: RegistryView) -> Vec<String> {
        match open_hklm_key(path, view) {
            Some(key) => key
                .enum_keys()
                // One unreadable subkey must not drop the others; it simply
                // does not appear.
                .filter_map(Result::ok)
                .collect(),
            None => Vec::new(),
        }
    }

    fn value(&self, path: &str, name: &str, view: RegistryView) -> Option<String> {
        open_hklm_key(path, view)?.get_value::<String, _>(name).ok()
    }
}

#[cfg(windows)]
fn open_hklm_key(path: &str, view: RegistryView) -> Option<winreg::RegKey> {
    use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY};

    // The WOW64 flags are only defined on a 64-bit OS; a 32-bit process on
    // 32-bit Windows opens keys plainly (there is no second view to read).
    // Redirection is explicitly requested either way so the *process*
    // architecture can never hide a vendor's registry data.
    let flags = if std::mem::size_of::<usize>() >= 8 {
        KEY_READ
            | match view {
                RegistryView::X64 => KEY_WOW64_64KEY,
                RegistryView::X32 => KEY_WOW64_32KEY,
            }
    } else {
        KEY_READ
    };
    winreg::RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags(path, flags)
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::java::{merge_java_installations, JavaArch, JavaInstallation};
    use std::collections::BTreeMap;

    // -------------------------------
    // Fake registry
    // -------------------------------

    #[derive(Default)]
    struct FakeKey {
        subkeys: Vec<String>,
        values: BTreeMap<String, String>,
    }

    impl FakeKey {
        fn sub(&mut self, name: &str) -> &mut Self {
            self.subkeys.push(name.to_string());
            self
        }
        fn val(&mut self, name: &str, value: &str) -> &mut Self {
            self.values.insert(name.to_string(), value.to_string());
            self
        }
    }

    /// HKLM model keyed by (view, key path). Missing entries model missing or
    /// inaccessible keys: reads return nothing, the walk continues.
    #[derive(Default)]
    struct FakeRegistry {
        keys: BTreeMap<(RegistryView, String), FakeKey>,
    }

    impl FakeRegistry {
        fn key(&mut self, view: RegistryView, path: &str) -> &mut FakeKey {
            self.keys.entry((view, path.to_string())).or_default()
        }
    }

    impl RegistryReader for FakeRegistry {
        fn subkeys(&self, path: &str, view: RegistryView) -> Vec<String> {
            self.keys
                .get(&(view, path.to_string()))
                .map(|k| k.subkeys.clone())
                .unwrap_or_default()
        }

        fn value(&self, path: &str, name: &str, view: RegistryView) -> Option<String> {
            self.keys
                .get(&(view, path.to_string()))?
                .values
                .get(name)
                .cloned()
        }
    }

    // -------------------------------
    // Fake filesystem for merge tests
    // -------------------------------

    /// A file that passes the PE sanity gate (MZ header, ≥10 KB) without
    /// being a real executable — the probe closure decides everything else.
    fn fake_java_file(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "voidlauncher-regjava-{}-{}.exe",
            std::process::id(),
            label
        ));
        let mut bytes = vec![0u8; 12_000];
        bytes[0] = b'M';
        bytes[1] = b'Z';
        std::fs::write(&path, bytes).expect("write fake java file");
        path
    }

    fn installation(path: &Path, major: u32) -> JavaInstallation {
        JavaInstallation {
            path: path.to_path_buf(),
            version: format!("{major}.0.0"),
            major_version: major,
            is_64bit: true,
            vendor: "Fake".to_string(),
            arch: JavaArch::X86_64,
        }
    }

    /// Register `path` as a chain: every segment becomes a subkey of the
    /// path before it. Keys deeper than the walked roots are unreachable
    /// without this — mirroring a real registry where each nesting level
    /// exists on its own.
    fn link_chain(reg: &mut FakeRegistry, view: RegistryView, path: &str) {
        let parts: Vec<&str> = path.split('\\').collect();
        for i in 1..parts.len() {
            reg.key(view, &parts[..i].join("\\")).sub(parts[i]);
        }
    }

    // -------------------------------
    // Registry walk tests
    // -------------------------------

    #[test]
    fn walk_finds_a_candidate_under_each_supported_root() {
        // (registry key, value name, stored value, expected executable)
        let vendors: &[(&str, &str, &str, &str)] = &[
            (
                r"SOFTWARE\JavaSoft\JDK",
                "JavaHome",
                "C:\\Jdks\\oracle17",
                "C:\\Jdks\\oracle17\\bin\\java.exe",
            ),
            (
                r"SOFTWARE\JavaSoft\Java Runtime Environment",
                "JavaHome",
                "C:\\Jdks\\oracle8",
                "C:\\Jdks\\oracle8\\bin\\java.exe",
            ),
            (
                r"SOFTWARE\Eclipse Adoptium\JDK\21.0.10.7\hotspot\MSI",
                "Path",
                "C:\\Jdks\\temurin21\\",
                "C:\\Jdks\\temurin21\\bin\\java.exe",
            ),
            (
                // Some records store the bin directory itself.
                r"SOFTWARE\Microsoft\JDK\21.0.2.13\hotspot\MSI",
                "Path",
                "C:\\Jdks\\msjdk21\\bin",
                "C:\\Jdks\\msjdk21\\bin\\java.exe",
            ),
            (
                r"SOFTWARE\Azul Systems\Zulu\zulu-11",
                "InstallationPath",
                "C:\\Jdks\\zulu11",
                "C:\\Jdks\\zulu11\\bin\\java.exe",
            ),
            (
                r"SOFTWARE\BellSoft\LibericaJDK\17",
                "JavaHome",
                "C:\\Jdks\\liberica",
                "C:\\Jdks\\liberica\\bin\\java.exe",
            ),
            (
                r"SOFTWARE\IBM\Semeru\OpenJDK\21",
                "JavaHome",
                "C:\\Jdks\\semeru21",
                "C:\\Jdks\\semeru21\\bin\\java.exe",
            ),
        ];
        for (key, name, value, expected) in vendors {
            let mut reg = FakeRegistry::default();
            link_chain(&mut reg, RegistryView::X64, key);
            reg.key(RegistryView::X64, key).val(name, value);
            let cands = registry_java_candidates(&reg);
            assert!(
                cands.contains(&PathBuf::from(*expected)),
                "root {key:?} must yield {expected:?}, got {cands:?}"
            );
        }

        // The JRE 8 layout additionally stores RuntimeLib, from which the
        // java.exe path is derived.
        let mut reg = FakeRegistry::default();
        link_chain(
            &mut reg,
            RegistryView::X64,
            r"SOFTWARE\JavaSoft\Java Runtime Environment\1.8",
        );
        reg.key(
            RegistryView::X64,
            r"SOFTWARE\JavaSoft\Java Runtime Environment\1.8",
        )
        .val("JavaHome", "C:\\Jdks\\oracle8")
        .val("RuntimeLib", "C:\\Jdks\\oracle8\\bin\\server\\jvm.dll");
        let cands = registry_java_candidates(&reg);
        assert!(cands.contains(&PathBuf::from(r"C:\Jdks\oracle8\bin\java.exe")));
        // RuntimeLib and JavaHome point at the same directory: no duplicate.
        assert_eq!(cands.len(), 1, "same dir via JavaHome+RuntimeLib dedupes");
    }

    #[test]
    fn walk_finds_multiple_versions_of_one_vendor() {
        let mut reg = FakeRegistry::default();
        link_chain(
            &mut reg,
            RegistryView::X64,
            r"SOFTWARE\Eclipse Adoptium\JDK\17.0.1.12\hotspot\MSI",
        );
        link_chain(
            &mut reg,
            RegistryView::X64,
            r"SOFTWARE\Eclipse Adoptium\JDK\21.0.10.7\hotspot\MSI",
        );
        for ver in ["17.0.1.12", "21.0.10.7"] {
            reg.key(
                RegistryView::X64,
                &format!(r"SOFTWARE\Eclipse Adoptium\JDK\{ver}\hotspot\MSI"),
            )
            .val(
                "Path",
                &format!("C:\\Program Files\\Eclipse Adoptium\\jdk-{ver}-hotspot\\"),
            )
            // The MSI flag set also writes DWORD values under the same names;
            // the fake only stores strings, mirroring "string reads win".
            .val("JavaHome", "1");
        }
        let cands = registry_java_candidates(&reg);
        assert_eq!(cands.len(), 2, "two vendor versions, got {cands:?}");
        assert!(cands.contains(&PathBuf::from(
            r"C:\Program Files\Eclipse Adoptium\jdk-17.0.1.12-hotspot\bin\java.exe"
        )));
        assert!(cands.contains(&PathBuf::from(
            r"C:\Program Files\Eclipse Adoptium\jdk-21.0.10.7-hotspot\bin\java.exe"
        )));
    }

    #[test]
    fn walk_reads_both_registry_views() {
        let mut reg = FakeRegistry::default();
        link_chain(&mut reg, RegistryView::X64, r"SOFTWARE\JavaSoft\JDK\17.0.5");
        link_chain(&mut reg, RegistryView::X32, r"SOFTWARE\JavaSoft\JDK\17.0.5");
        // 64-bit view: only vendor A.
        reg.key(RegistryView::X64, r"SOFTWARE\JavaSoft\JDK\17.0.5")
            .val("JavaHome", "C:\\Jdks\\jdk17-64");
        // 32-bit view (Wow6432Node on disk): a separate 32-bit registration.
        reg.key(RegistryView::X32, r"SOFTWARE\JavaSoft\JDK\17.0.5")
            .val("JavaHome", "C:\\Jdks\\jdk17-32");
        let cands = registry_java_candidates(&reg);
        assert!(
            cands.contains(&PathBuf::from(r"C:\Jdks\jdk17-64\bin\java.exe")),
            "64-bit view content found"
        );
        assert!(
            cands.contains(&PathBuf::from(r"C:\Jdks\jdk17-32\bin\java.exe")),
            "32-bit view (Wow6432Node) content found"
        );

        // A duplicate across views (both views reflect the same home) must
        // not double-count: registry-level dedup handles it.
        // The "17.0.5x" duplicate key needs its own chain link.
        link_chain(
            &mut reg,
            RegistryView::X64,
            r"SOFTWARE\JavaSoft\JDK\17.0.5x",
        );
        reg.key(RegistryView::X64, r"SOFTWARE\JavaSoft\JDK\17.0.5x")
            .val("JavaHome", "C:\\Jdks\\jdk17-64");
        let cands = registry_java_candidates(&reg);
        assert_eq!(
            cands
                .iter()
                .filter(|c| c
                    .to_string_lossy()
                    .to_ascii_lowercase()
                    .contains("jdk17-64"))
                .count(),
            1
        );
    }

    #[test]
    fn walk_continues_when_a_branch_is_unreadable() {
        let mut reg = FakeRegistry::default();
        link_chain(
            &mut reg,
            RegistryView::X64,
            r"SOFTWARE\Eclipse Adoptium\JDK\21",
        );
        link_chain(
            &mut reg,
            RegistryView::X64,
            r"SOFTWARE\IBM\Semeru\OpenJDK\21",
        );
        // Six of eight roots exist; a missing root models a key the process
        // cannot open (absent access, corrupted, removed mid-scan).
        reg.key(RegistryView::X64, r"SOFTWARE\Eclipse Adoptium\JDK\21")
            .val("JavaHome", "C:\\Jdks\\temurin21");
        reg.key(RegistryView::X64, r"SOFTWARE\IBM\Semeru\OpenJDK\21")
            .val("JavaHome", "C:\\Jdks\\semeru21");
        // Every other root is missing from the map entirely — the walk must
        // still deliver the two existing vendors.
        let cands = registry_java_candidates(&reg);
        assert_eq!(cands.len(), 2, "got {cands:?}");
        assert!(cands.contains(&PathBuf::from(r"C:\Jdks\temurin21\bin\java.exe")));
        assert!(cands.contains(&PathBuf::from(r"C:\Jdks\semeru21\bin\java.exe")));
    }

    #[test]
    fn walk_ignores_keys_without_usable_home_values() {
        let mut reg = FakeRegistry::default();
        // Key present, subkeys present, but no home value anywhere.
        reg.key(RegistryView::X64, r"SOFTWARE\Eclipse Adoptium\JDK")
            .sub("17.0.1.12");
        reg.key(
            RegistryView::X64,
            r"SOFTWARE\Eclipse Adoptium\JDK\17.0.1.12",
        )
        .sub("hotspot");
        reg.key(
            RegistryView::X64,
            r"SOFTWARE\Eclipse Adoptium\JDK\17.0.1.12\hotspot",
        )
        .sub("MSI");
        assert!(
            registry_java_candidates(&reg).is_empty(),
            "no home value must yield no candidate"
        );

        // A home value that is empty or relative is not usable either.
        reg.key(
            RegistryView::X64,
            r"SOFTWARE\Eclipse Adoptium\JDK\17.0.1.12\hotspot\MSI",
        )
        .val("Path", "");
        assert!(registry_java_candidates(&reg).is_empty());
    }

    #[test]
    fn candidates_are_deterministic_and_case_deduped() {
        let mut reg = FakeRegistry::default();
        // The same home written twice with different letters (Windows paths
        // are case-insensitive).
        link_chain(&mut reg, RegistryView::X64, r"SOFTWARE\JavaSoft\JDK\17");
        link_chain(&mut reg, RegistryView::X64, r"SOFTWARE\JavaSoft\JRE\17");
        reg.key(RegistryView::X64, r"SOFTWARE\JavaSoft\JDK\17")
            .val("JavaHome", "C:\\JDKS\\ORACLE17");
        reg.key(RegistryView::X64, r"SOFTWARE\JavaSoft\JRE\17")
            .val("JavaHome", "C:\\Jdks\\oracle17");
        let cands = registry_java_candidates(&reg);
        assert_eq!(
            cands
                .iter()
                .filter(|c| *c == &PathBuf::from(r"C:\JDKS\ORACLE17\bin\java.exe"))
                .count(),
            1
        );
        // First-seen order is stable across calls with the same reader.
        let again = registry_java_candidates(&reg);
        assert_eq!(cands, again);
    }

    // -------------------------------
    // Merge tests (dedup + probe + sort)
    // -------------------------------

    #[test]
    fn merge_deduplicates_one_executable_across_sources() {
        let exe = fake_java_file("merge-dedup");
        // Same executable discovered three times: registry (as written),
        // PATH (different letter case), PATH (forward slashes) — all
        // collapse to one probed installation.
        let variants = vec![
            exe.clone(),
            PathBuf::from(exe.to_string_lossy().replace('\\', "/")),
            PathBuf::from(exe.to_string_lossy().to_uppercase()),
        ];
        let probe_calls = std::cell::Cell::new(0u32);
        let probed = |p: &Path| {
            probe_calls.set(probe_calls.get() + 1);
            Some(installation(p, 21))
        };
        let out = merge_java_installations(variants, probed);
        assert_eq!(out.len(), 1, "one canonical executable");
        assert_eq!(
            probe_calls.get(),
            1,
            "probe runs once per unique executable"
        );
        assert_eq!(out[0].path, exe);
        std::fs::remove_file(&exe).ok();
    }

    #[test]
    fn merge_skips_missing_and_non_executable_candidates() {
        let real = fake_java_file("merge-real");
        // PE bytes missing (not a Windows executable) — must be rejected
        // before any probe run, however plausible the registry record is.
        let not_pe = std::env::temp_dir().join(format!(
            "voidlauncher-regjava-{}-text.bin",
            std::process::id()
        ));
        std::fs::write(&not_pe, vec![0u8; 12_000]).ok();

        let candidates = vec![
            real.clone(),
            PathBuf::from(r"C:\definitely\missing\bin\java.exe"),
            not_pe.clone(),
        ];
        let probe_calls = std::cell::Cell::new(0u32);
        let probed = |p: &Path| {
            probe_calls.set(probe_calls.get() + 1);
            Some(installation(p, 17))
        };
        let out = merge_java_installations(candidates, probed);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].path, real);
        assert_eq!(probe_calls.get(), 1, "only the real executable is probed");
        std::fs::remove_file(&real).ok();
        std::fs::remove_file(&not_pe).ok();
    }

    #[test]
    fn merge_probes_are_fatal_when_the_jvm_is_invalid() {
        let exe = fake_java_file("merge-invalid");
        let candidates = vec![exe.clone()];
        let out = merge_java_installations(candidates, |_p| None);
        assert!(out.is_empty(), "a failed probe excludes the candidate");
        std::fs::remove_file(&exe).ok();
    }

    #[test]
    fn merge_sorts_major_then_bitness_then_path() {
        let a = fake_java_file("merge-a");
        let b = fake_java_file("merge-b");
        let c = fake_java_file("merge-c");
        let d = fake_java_file("merge-d");
        let mut candidates = vec![b.clone(), a.clone(), c.clone(), d.clone()];
        // Deterministic shuffle so sorting, not input order, decides.
        candidates.rotate_left(2);

        let probed = |p: &Path| {
            let name = p.file_stem().unwrap().to_string_lossy().to_string();
            let (major, is_64bit) = if name.ends_with("-merge-a") {
                (17, true)
            } else if name.ends_with("-merge-b") {
                (21, true)
            } else if name.ends_with("-merge-c") {
                (17, false)
            } else {
                (17, true) // merge-d: same major/bits as a → path tiebreak
            };
            Some(JavaInstallation {
                is_64bit,
                arch: JavaArch::Unknown,
                ..installation(p, major)
            })
        };
        let out = merge_java_installations(candidates, probed);

        let labels: Vec<String> = out
            .iter()
            .map(|i| {
                let name = i.path.file_stem().unwrap().to_string_lossy().to_string();
                ["a", "b", "c", "d"]
                    .iter()
                    .find(|l| name.ends_with(&format!("-merge-{l}")))
                    .map(|l| (*l).to_string())
                    .expect("test file name")
            })
            .collect();
        assert_eq!(labels, vec!["b", "a", "d", "c"]);
        for f in [&a, &b, &c, &d] {
            std::fs::remove_file(f).ok();
        }
    }
}

//! Persistent installer metadata and transactional publication for generated
//! Forge/NeoForge processor outputs.
//!
//! This module deliberately distinguishes local provenance from publisher
//! authentication. The manifest records which verified installer, processor
//! inputs and output files were used; it is not a signature and cannot stop a
//! malicious process running in the same Windows user context from rewriting
//! both the manifest and the files.

use crate::error::{LauncherError, Result};
use crate::integrity::HashSpec;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use uuid::Uuid;

pub(crate) const MANIFEST_SCHEMA_VERSION: u32 = 1;
pub(crate) const IDENTITY_SCHEMA_VERSION: u32 = 1;
pub(crate) const JOURNAL_SCHEMA_VERSION: u32 = 2;

const MAX_JSON_BYTES: u64 = 4 * 1024 * 1024;
/// A journal with more entries than any real Forge/NeoForge installer needs is
/// treated as corrupt before any path is touched.
const MAX_JOURNAL_ENTRIES: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FileDigest {
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct InstallerIdentity {
    pub schema_version: u32,
    pub loader: String,
    pub mc_version: String,
    pub loader_version: String,
    pub sha256: String,
    pub size: u64,
    pub checksum_algorithm: String,
    pub checksum_hex: String,
    pub checksum_source: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ProcessorLibraryIdentity {
    pub name: String,
    pub relative_path: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ProcessorIdentity {
    pub jar: String,
    pub classpath: Vec<String>,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct OutputIdentity {
    pub relative_path: String,
    pub sha256: String,
    pub size: u64,
    pub publisher_sha1: Option<String>,
    pub structural_only: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct GenerationManifest {
    pub schema_version: u32,
    pub loader: String,
    pub mc_version: String,
    pub loader_version: String,
    pub installer: InstallerIdentity,
    pub install_profile_sha256: String,
    pub client_jar: FileDigest,
    pub processor_libraries: Vec<ProcessorLibraryIdentity>,
    pub processors: Vec<ProcessorIdentity>,
    pub outputs: Vec<OutputIdentity>,
}

#[derive(Debug, Clone)]
pub(crate) struct ExpectedOutput {
    pub target: PathBuf,
    pub relative_path: String,
    pub publisher_sha1: Option<String>,
    pub structural_only: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct OutputSpec {
    pub target: PathBuf,
    pub staged: PathBuf,
    pub publisher_sha1: Option<String>,
    pub structural_only: bool,
}

/// Read-only verdict for a loader generation. `Healthy` is returned only when
/// every required input, output and embedded artifact was checked successfully;
/// "not checked" is never reported as healthy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GenerationState {
    Healthy,
    NeedsRepair,
    NeedsMigration,
    RecoveryRequired,
    Busy,
    Incomplete,
}

#[derive(Debug, Clone)]
pub(crate) struct GenerationIssue {
    pub path: String,
    pub kind: String,
    pub detail: Option<String>,
    pub repairable: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct GenerationInspection {
    pub state: GenerationState,
    pub issues: Vec<GenerationIssue>,
}

impl GenerationInspection {
    pub(crate) fn new(state: GenerationState) -> Self {
        Self {
            state,
            issues: Vec::new(),
        }
    }

    pub(crate) fn push(
        &mut self,
        path: impl Into<String>,
        kind: &str,
        detail: Option<String>,
        repairable: bool,
    ) {
        if self.state == GenerationState::Healthy {
            self.state = GenerationState::NeedsRepair;
        }
        self.issues.push(GenerationIssue {
            path: path.into(),
            kind: kind.to_string(),
            detail,
            repairable,
        });
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TransactionState {
    Clean,
    Prepared,
    CommittedValid,
    CommittedDamaged,
}

#[derive(Debug, Clone)]
pub(crate) struct TransactionInspection {
    pub state: TransactionState,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum JournalStage {
    Prepared,
    Committed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct JournalEntry {
    target: PathBuf,
    staged: PathBuf,
    backup: PathBuf,
    existed: bool,
    old_sha256: Option<String>,
    new_sha256: String,
    publisher_sha1: Option<String>,
    structural_only: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct TransactionJournal {
    schema_version: u32,
    tx_id: String,
    generation: u64,
    stage: JournalStage,
    entries: Vec<JournalEntry>,
    /// Hash of every other journal field. This detects a valid-JSON bit flip;
    /// it is an integrity check against accidental corruption, not a MAC and
    /// not a defence against a process running as the same user.
    content_sha256: String,
}

#[derive(Serialize)]
struct JournalContent<'a> {
    schema_version: u32,
    tx_id: &'a str,
    generation: u64,
    stage: &'a JournalStage,
    entries: &'a [JournalEntry],
}

impl TransactionJournal {
    fn new(entries: Vec<JournalEntry>) -> Self {
        Self {
            schema_version: JOURNAL_SCHEMA_VERSION,
            tx_id: Uuid::new_v4().to_string(),
            generation: 0,
            stage: JournalStage::Prepared,
            entries,
            content_sha256: String::new(),
        }
    }

    fn seal(&mut self) -> Result<()> {
        let bytes = serde_json::to_vec(&JournalContent {
            schema_version: self.schema_version,
            tx_id: &self.tx_id,
            generation: self.generation,
            stage: &self.stage,
            entries: &self.entries,
        })
        .map_err(|e| {
            LauncherError::ModLoader(format!(
                "Failed to serialize processor journal content: {e}"
            ))
        })?;
        self.content_sha256 = sha256_bytes(&bytes);
        Ok(())
    }

    fn verify_seal(&self) -> Result<()> {
        let bytes = serde_json::to_vec(&JournalContent {
            schema_version: self.schema_version,
            tx_id: &self.tx_id,
            generation: self.generation,
            stage: &self.stage,
            entries: &self.entries,
        })
        .map_err(|e| {
            LauncherError::ModLoader(format!(
                "Failed to serialize processor journal content: {e}"
            ))
        })?;
        let expected = sha256_bytes(&bytes);
        if self.content_sha256 != expected {
            return Err(LauncherError::ModLoader(format!(
                "Processor transaction journal content hash does not match ({} != {})",
                self.content_sha256, expected
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum InstallerTestStage {
    AfterLock,
}

#[cfg(test)]
type InstallerTestHook = std::sync::Arc<dyn Fn(&ProcessorCache, InstallerTestStage) + Send + Sync>;

#[cfg(test)]
#[derive(Clone, Default)]
struct InstallerTestHookSlot(Arc<StdMutex<Option<InstallerTestHook>>>);

#[cfg(test)]
impl std::fmt::Debug for InstallerTestHookSlot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("InstallerTestHookSlot")
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ProcessorCache {
    pub data_dir: PathBuf,
    pub loader: String,
    pub mc_version: String,
    pub loader_version: String,
    pub root: PathBuf,
    pub installer: PathBuf,
    pub installer_identity: PathBuf,
    pub generation_manifest: PathBuf,
    pub tx_dir: PathBuf,
    pub staging_dir: PathBuf,
    pub backup_dir: PathBuf,
    pub journal: PathBuf,
    pub journal_previous: PathBuf,
    /// Test-only hook для доказательства того, что публикация installer
    /// выполняется под operation lock; в production отсутствует.
    #[cfg(test)]
    test_installer_hook: InstallerTestHookSlot,
}

/// Process-wide serialization for one loader-cache root.
///
/// Install, repair and launch preparation may run for different instances
/// that share the same loader version, so the per-instance launch registry is
/// not enough. The single-instance mutex in `lib.rs` prevents a second
/// launcher process; this map prevents two operations inside one process from
/// entering the same `tx/` directory.
static OPERATION_LOCKS: OnceLock<StdMutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> =
    OnceLock::new();

impl ProcessorCache {
    pub(crate) fn new(
        data_dir: &Path,
        loader: &str,
        mc_version: &str,
        loader_version: &str,
    ) -> Result<Self> {
        for (value, label) in [
            (loader, "loader"),
            (mc_version, "Minecraft version"),
            (loader_version, "loader version"),
        ] {
            if crate::import::check_safe_file_name(value).is_err() {
                return Err(LauncherError::ModLoader(format!(
                    "Unsafe {label} for loader cache: {value:?}"
                )));
            }
        }
        if !data_dir.is_absolute() {
            return Err(LauncherError::ModLoader(format!(
                "Loader cache root must be absolute: {}",
                data_dir.display()
            )));
        }
        let root = data_dir
            .join("loader-cache")
            .join(loader)
            .join(mc_version)
            .join(loader_version);
        if !root.starts_with(data_dir) {
            return Err(LauncherError::ModLoader(
                "Loader cache path escapes the data directory".into(),
            ));
        }
        Ok(Self {
            data_dir: data_dir.to_path_buf(),
            loader: loader.to_string(),
            mc_version: mc_version.to_string(),
            loader_version: loader_version.to_string(),
            installer: root.join("installer.jar"),
            installer_identity: root.join("installer.json"),
            generation_manifest: root.join("generation.json"),
            tx_dir: root.join("tx"),
            staging_dir: root.join("tx").join("staging"),
            backup_dir: root.join("tx").join("backup"),
            journal: root.join("tx").join("journal.json"),
            journal_previous: root.join("tx").join("journal.prev.json"),
            root,
            #[cfg(test)]
            test_installer_hook: InstallerTestHookSlot::default(),
        })
    }

    #[cfg(test)]
    fn run_installer_test_hook(&self, stage: InstallerTestStage) {
        let hook = self
            .test_installer_hook
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        if let Some(hook) = hook {
            hook(self, stage);
        }
    }

    #[cfg(test)]
    fn set_test_installer_hook(&self, hook: Option<InstallerTestHook>) {
        *self
            .test_installer_hook
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = hook;
    }

    /// Serialize every operation that can stage, publish or recover outputs in
    /// this cache root. Callers must hold this guard for the complete
    /// stage -> publish sequence, not only for individual filesystem calls.
    ///
    /// The operation lock is process-local; a second launcher process is
    /// already refused by `single_instance`. A file lock would add a second,
    /// weaker authority without covering the same-user attacker this design
    /// does not claim to stop.
    pub(crate) async fn acquire_operation_lock(&self) -> tokio::sync::OwnedMutexGuard<()> {
        let key = path_identity(&self.root);
        let lock = {
            let mut locks = OPERATION_LOCKS
                .get_or_init(|| StdMutex::new(HashMap::new()))
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            locks
                .entry(key)
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        lock.lock_owned().await
    }

    /// Non-blocking variant for read-only inspection. The caller must not use
    /// this to replace the normal mutation lock; it only lets Verify return
    /// `Busy` instead of reading a transaction while publication is in flight.
    /// Run one blocking processor worker while transferring ownership of the
    /// cache operation guard into that worker.
    ///
    /// A Tokio `spawn_blocking` task cannot be aborted once it has started. If
    /// the outer command future is dropped, the guard must therefore stay with
    /// the worker until the worker returns; it must not be held only by the
    /// async frame. The guard is returned on success so the caller can keep the
    /// same serialization across publication and manifest writes.
    pub(crate) async fn run_with_owned_operation_lock<F, R>(
        &self,
        lock: tokio::sync::OwnedMutexGuard<()>,
        worker: F,
    ) -> Result<(R, tokio::sync::OwnedMutexGuard<()>)>
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        match tokio::task::spawn_blocking(move || {
            let result = worker();
            (result, lock)
        })
        .await
        {
            Ok(value) => Ok(value),
            Err(error) => Err(LauncherError::ModLoader(format!(
                "Processor worker failed while holding the loader cache lock: {error}"
            ))),
        }
    }

    pub(crate) fn try_acquire_operation_lock(&self) -> Option<tokio::sync::OwnedMutexGuard<()>> {
        let key = path_identity(&self.root);
        let lock = {
            let mut locks = OPERATION_LOCKS
                .get_or_init(|| StdMutex::new(HashMap::new()))
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            locks
                .entry(key)
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        lock.try_lock_owned().ok()
    }

    #[cfg(test)]
    fn operation_lock_is_held(&self) -> bool {
        let key = path_identity(&self.root);
        let lock = {
            let locks = OPERATION_LOCKS
                .get_or_init(|| StdMutex::new(HashMap::new()))
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            locks.get(&key).cloned()
        };
        lock.map(|lock| lock.try_lock().is_err()).unwrap_or(false)
    }

    pub(crate) fn prepare(&self) -> Result<()> {
        // Check every existing component before creating anything: a junction
        // at loader-cache/... must not receive newly created directories just
        // because the check ran after create_dir_all.
        crate::import::ensure_no_reparse_points(&self.data_dir, &self.root)?;
        std::fs::create_dir_all(&self.root)?;
        crate::import::ensure_no_reparse_points(&self.root, &self.tx_dir)?;
        std::fs::create_dir_all(&self.tx_dir)?;
        // Re-check after creation so a junction that already existed below the
        // root cannot be mistaken for a directory this call created.
        crate::import::ensure_no_reparse_points(&self.data_dir, &self.root)?;
        crate::import::ensure_no_reparse_points(&self.root, &self.tx_dir)?;
        Ok(())
    }

    /// Проверяет/скачивает installer и публикует его identity под тем же
    /// per-cache operation lock, что используется для processor publication.
    pub(crate) async fn ensure_installer(
        &self,
        candidates: &[String],
        allowed_hosts: &[&str],
        label: &str,
    ) -> Result<super::artifact::InstallerVerification> {
        let _lock = self.acquire_operation_lock().await;
        #[cfg(test)]
        self.run_installer_test_hook(InstallerTestStage::AfterLock);
        self.ensure_installer_locked(candidates, allowed_hosts, label)
            .await
    }

    /// Вариант для вызывающих, которые уже владеют per-cache operation lock.
    /// Lock должен покрывать проверку identity, публикацию installer и запись
    /// соответствующего `installer.json`, чтобы две операции не опубликовали
    /// разные generation одного cache одновременно.
    pub(crate) async fn ensure_installer_locked(
        &self,
        candidates: &[String],
        allowed_hosts: &[&str],
        label: &str,
    ) -> Result<super::artifact::InstallerVerification> {
        self.prepare()?;
        if let Some(identity) = self.load_installer_identity()? {
            if self.installer_identity_matches(&identity)? {
                return identity.into_verification();
            }
        }
        // A cached installer without a matching provenance record is not
        // trusted. Re-verify/download it from the publisher sidecar.
        let verification = super::artifact::download_verified_installer(
            candidates,
            &self.installer,
            allowed_hosts,
            label,
        )
        .await?;
        let identity = self.identity_from_verification(&verification)?;
        self.write_installer_identity(&identity)?;
        Ok(verification)
    }

    pub(crate) fn load_installer_identity(&self) -> Result<Option<InstallerIdentity>> {
        let Some(bytes) = read_small_json(&self.installer_identity)? else {
            return Ok(None);
        };
        let identity: InstallerIdentity = match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            Err(e) => {
                tracing::warn!(
                    target: "launcher",
                    "Ignoring malformed installer provenance at {}: {}",
                    self.installer_identity.display(),
                    e
                );
                return Ok(None);
            }
        };
        if identity.schema_version != IDENTITY_SCHEMA_VERSION {
            return Ok(None);
        }
        Ok(Some(identity))
    }

    pub(crate) fn installer_identity_matches(&self, identity: &InstallerIdentity) -> Result<bool> {
        if identity.schema_version != IDENTITY_SCHEMA_VERSION
            || identity.loader != self.loader
            || identity.mc_version != self.mc_version
            || identity.loader_version != self.loader_version
            || !self.installer.is_file()
            || identity.size == 0
        {
            return Ok(false);
        }
        let digest = file_digest(&self.installer)?;
        if digest.size != identity.size || !digest.sha256.eq_ignore_ascii_case(&identity.sha256) {
            return Ok(false);
        }
        // A syntactically valid installer.json with a malformed checksum is
        // damaged local metadata, not a reason to strand the cache. The
        // record is marked untrusted and the normal re-download path runs;
        // malformed publisher data is still never accepted as proof.
        let hash = match HashSpec::parse(&identity.checksum_algorithm, &identity.checksum_hex) {
            Ok(hash) => hash,
            Err(error) => {
                tracing::warn!(
                    target: "launcher",
                    "Ignoring malformed installer provenance checksum at {}: {}",
                    self.installer_identity.display(),
                    error
                );
                return Ok(false);
            }
        };
        match hash.verify_file(&self.installer) {
            Ok(ok) => Ok(ok),
            // A read/permission failure is not malformed metadata and must not
            // be silently converted into a re-download.
            Err(error) => Err(error),
        }
    }

    fn identity_from_verification(
        &self,
        verification: &super::artifact::InstallerVerification,
    ) -> Result<InstallerIdentity> {
        let digest = file_digest(&self.installer)?;
        Ok(InstallerIdentity {
            schema_version: IDENTITY_SCHEMA_VERSION,
            loader: self.loader.clone(),
            mc_version: self.mc_version.clone(),
            loader_version: self.loader_version.clone(),
            sha256: digest.sha256,
            size: digest.size,
            checksum_algorithm: verification.checksum.algorithm().to_string(),
            checksum_hex: verification.checksum.hex().to_string(),
            checksum_source: verification.source.clone(),
            url: verification.url.clone(),
        })
    }

    pub(crate) fn write_installer_identity(&self, identity: &InstallerIdentity) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(identity).map_err(|e| {
            LauncherError::ModLoader(format!("Failed to serialize installer provenance: {e}"))
        })?;
        crate::instances::write_file_atomic(&self.installer_identity, &bytes)
    }

    pub(crate) fn load_generation_manifest(&self) -> Result<Option<GenerationManifest>> {
        // A pending transaction means the files on disk may be a mixture of
        // old and new outputs. Never let a stale manifest legitimise them;
        // callers run recovery before this function, so a journal still being
        // present here is an error rather than a cache miss.
        if self.pending_journal_exists() {
            return Err(LauncherError::ModLoader(
                "A pending processor transaction must be recovered before the generation manifest is trusted"
                    .into(),
            ));
        }
        let Some(bytes) = read_small_json(&self.generation_manifest)? else {
            return Ok(None);
        };
        let manifest: GenerationManifest = match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            Err(e) => {
                tracing::warn!(
                    target: "launcher",
                    "Ignoring malformed generation provenance at {}: {}",
                    self.generation_manifest.display(),
                    e
                );
                return Ok(None);
            }
        };
        if manifest.schema_version != MANIFEST_SCHEMA_VERSION {
            return Ok(None);
        }
        Ok(Some(manifest))
    }

    pub(crate) fn write_generation_manifest(&self, manifest: &GenerationManifest) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(manifest).map_err(|e| {
            LauncherError::ModLoader(format!("Failed to serialize generation provenance: {e}"))
        })?;
        crate::instances::write_file_atomic(&self.generation_manifest, &bytes)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn validate_generation_manifest(
        &self,
        manifest: &GenerationManifest,
        loader: &str,
        mc_version: &str,
        loader_version: &str,
        installer: &InstallerIdentity,
        install_profile_sha256: &str,
        client_jar: &Path,
        libraries_dir: &Path,
        processors: &[ProcessorIdentity],
        processor_libraries: &[ProcessorLibraryIdentity],
        expected_outputs: &[ExpectedOutput],
    ) -> Result<()> {
        if manifest.schema_version != MANIFEST_SCHEMA_VERSION
            || manifest.loader != loader
            || manifest.mc_version != mc_version
            || manifest.loader_version != loader_version
        {
            return Err(stale("loader/version identity"));
        }
        if self.pending_journal_exists() {
            return Err(stale("pending processor transaction"));
        }
        let mut manifest_paths = HashSet::new();
        for output in &manifest.outputs {
            if !manifest_paths.insert(relative_identity(&output.relative_path)) {
                return Err(stale("duplicate generated output record"));
            }
        }
        if manifest.installer.sha256 != installer.sha256
            || manifest.installer.size != installer.size
            || manifest.installer.checksum_algorithm != installer.checksum_algorithm
            || manifest.installer.checksum_hex != installer.checksum_hex
            || manifest.installer.checksum_source != installer.checksum_source
            || manifest.installer.url != installer.url
        {
            return Err(stale("installer identity"));
        }
        if manifest.install_profile_sha256 != install_profile_sha256 {
            return Err(stale("install_profile hash"));
        }
        let client = file_digest(client_jar)?;
        if client.sha256 != manifest.client_jar.sha256 || client.size != manifest.client_jar.size {
            return Err(stale("client.jar hash"));
        }
        if manifest.processors != processors {
            return Err(stale("processor identities"));
        }
        if manifest.processor_libraries != processor_libraries {
            return Err(stale("processor library set"));
        }
        let mut seen_libraries = HashSet::new();
        for library in &manifest.processor_libraries {
            if !seen_libraries.insert(relative_identity(&library.relative_path)) {
                return Err(stale("duplicate processor library"));
            }
            let path = checked_relative(libraries_dir, &library.relative_path)?;
            let digest = file_digest(&path)?;
            if digest.sha256 != library.sha256 || digest.size != library.size {
                return Err(stale(&format!(
                    "processor library {} changed",
                    library.name
                )));
            }
        }
        if manifest.outputs.len() != expected_outputs.len() {
            return Err(stale("generated output set"));
        }
        let mut seen_outputs = HashSet::new();
        for expected in expected_outputs {
            if !seen_outputs.insert(path_identity(&expected.target)) {
                return Err(stale("duplicate generated output"));
            }
            let Some(record) = manifest.outputs.iter().find(|output| {
                relative_identity(&output.relative_path)
                    == relative_identity(&expected.relative_path)
            }) else {
                return Err(stale("missing generated output record"));
            };
            if record.publisher_sha1 != expected.publisher_sha1
                || record.structural_only != expected.structural_only
            {
                return Err(stale("generated output provenance"));
            }
            checked_journal_path(libraries_dir, &expected.target, "generated output target")?;
            let digest = file_digest(&expected.target)?;
            if digest.sha256 != record.sha256 || digest.size != record.size {
                return Err(stale("generated output file changed"));
            }
            if let Some(publisher_sha1) = &record.publisher_sha1 {
                let hash = HashSpec::sha1(publisher_sha1)?;
                if !hash.verify_file(&expected.target)? {
                    return Err(stale("generated output publisher checksum"));
                }
            }
        }
        Ok(())
    }

    /// Prepare `tx/staging` for one processor run.
    ///
    /// The caller must hold [`ProcessorCache::acquire_operation_lock`] for the
    /// whole stage -> publish sequence so another instance cannot delete or
    /// publish the same staging directory.
    pub(crate) fn begin_staging(&self) -> Result<()> {
        self.prepare()?;
        // Never delete staging data that a pending transaction may still need
        // for rollback. The caller must run recovery first.
        if self.pending_journal_exists() {
            return Err(LauncherError::ModLoader(
                "An unfinished processor transaction must be recovered first".into(),
            ));
        }
        crate::import::ensure_no_reparse_points(&self.tx_dir, &self.staging_dir)?;
        crate::import::ensure_no_reparse_points(&self.tx_dir, &self.backup_dir)?;
        if self.staging_dir.exists() {
            std::fs::remove_dir_all(&self.staging_dir)?;
        }
        std::fs::create_dir_all(&self.staging_dir)?;
        crate::import::ensure_no_reparse_points(&self.tx_dir, &self.staging_dir)?;
        if self.backup_dir.exists() {
            std::fs::remove_dir_all(&self.backup_dir)?;
        }
        Ok(())
    }

    /// Roll back or finish a transaction left by a crash.
    ///
    /// The caller must hold [`ProcessorCache::acquire_operation_lock`].
    pub(crate) fn recover_pending_transaction(&self, libraries_dir: &Path) -> Result<()> {
        let Some(journal) = self.load_journal()? else {
            return Ok(());
        };
        self.validate_journal_paths(&journal, libraries_dir)?;
        match journal.stage {
            JournalStage::Prepared => {
                self.rollback(&journal, libraries_dir)?;
                self.cleanup_tx()?;
                Ok(())
            }
            JournalStage::Committed => {
                if self.committed_entries_are_current(&journal) {
                    self.cleanup_tx()?;
                    return Ok(());
                }
                // A committed transaction is already final. Backups may have
                // been partially deleted by cleanup, so a rollback could
                // restore only some old outputs and leave a mixed set. Do not
                // touch any target or remaining backup here: report Recovery
                // Required so the caller can regenerate from verified inputs.
                Err(self.committed_generation_damaged_error(&journal))
            }
        }
    }

    /// Read-only transaction classification for Verify. This never rolls back,
    /// cleans up or touches backups; callers must hold a read-side operation
    /// lock or accept that `Busy` is possible.
    pub(crate) fn inspect_transaction(
        &self,
        libraries_dir: &Path,
    ) -> Result<TransactionInspection> {
        let Some(journal) = self.load_journal()? else {
            return Ok(TransactionInspection {
                state: TransactionState::Clean,
                detail: None,
            });
        };
        self.validate_journal_paths(&journal, libraries_dir)?;
        match journal.stage {
            JournalStage::Prepared => Ok(TransactionInspection {
                state: TransactionState::Prepared,
                detail: Some(format!(
                    "transaction {} has a prepared journal and must be rolled back",
                    journal.tx_id
                )),
            }),
            JournalStage::Committed => {
                if self.committed_entries_are_current(&journal) {
                    Ok(TransactionInspection {
                        state: TransactionState::CommittedValid,
                        detail: Some(format!(
                            "transaction {} is committed; cleanup is still pending",
                            journal.tx_id
                        )),
                    })
                } else {
                    Ok(TransactionInspection {
                        state: TransactionState::CommittedDamaged,
                        detail: Some(format!(
                            "committed transaction {} has damaged outputs and cannot be rolled back safely",
                            journal.tx_id
                        )),
                    })
                }
            }
        }
    }

    fn committed_entries_are_current(&self, journal: &TransactionJournal) -> bool {
        journal.entries.iter().all(|entry| {
            if !entry.target.is_file() || !super::forge::is_usable_jar(&entry.target) {
                return false;
            }
            match file_digest(&entry.target) {
                Ok(digest) if digest.sha256 == entry.new_sha256 => {}
                _ => return false,
            }
            if let Some(publisher_sha1) = &entry.publisher_sha1 {
                match HashSpec::sha1(publisher_sha1)
                    .and_then(|hash| hash.verify_file(&entry.target))
                {
                    Ok(true) => {}
                    _ => return false,
                }
            }
            true
        })
    }

    fn committed_generation_damaged_error(&self, journal: &TransactionJournal) -> LauncherError {
        LauncherError::ModLoader(format!(
            "Committed processor generation {} is damaged and cannot be rolled back safely: \
             backups may have been partially cleaned. Recovery is required; the instance cannot launch. \
             Journal: {}; preserved backups: {}; staging: {}. Do not delete these files. \
             Regenerate the loader outputs from the verified installer after preserving this transaction.",
            journal.tx_id,
            self.journal.display(),
            self.backup_dir.display(),
            self.staging_dir.display()
        ))
    }

    /// Publish a verified processor output set.
    ///
    /// Contract: the caller must hold
    /// [`ProcessorCache::acquire_operation_lock`] for the whole stage ->
    /// publish sequence, and every `staged` file must already have been
    /// produced by the processor pipeline. Publication independently verifies
    /// each staged JAR's structure, local digest and publisher checksum (when
    /// declared), then re-verifies the published target after the rename.
    pub(crate) fn publish_outputs(
        &self,
        libraries_dir: &Path,
        outputs: &[OutputSpec],
    ) -> Result<()> {
        self.recover_pending_transaction(libraries_dir)?;
        if outputs.is_empty() {
            return Err(LauncherError::ModLoader(
                "Processor pipeline produced no declared outputs".into(),
            ));
        }
        self.prepare()?;
        std::fs::create_dir_all(&self.backup_dir)?;
        crate::import::ensure_no_reparse_points(&self.tx_dir, &self.backup_dir)?;
        crate::import::ensure_no_reparse_points(&self.tx_dir, &self.staging_dir)?;
        let entries = self.prepare_journal_entries(libraries_dir, outputs)?;
        let mut journal = TransactionJournal::new(entries);
        journal.generation = 1;
        // Reject unsafe paths, duplicate targets/staging/backups and invalid
        // hashes before the first old target is moved.
        self.validate_journal_paths(&journal, libraries_dir)?;
        // Cross-volume publication copies into the target's directory, so
        // the directory must exist before the transaction starts. Creating
        // empty directories is harmless and leaves targets untouched.
        for entry in &journal.entries {
            prepare_target_parent(libraries_dir, &entry.target)?;
        }
        self.write_journal(&mut journal)?;
        test_failpoint("before_first_publish");

        let publish_result = (|| -> Result<()> {
            for (index, entry) in journal.entries.iter().enumerate() {
                if entry.existed {
                    let old_sha256 = entry.old_sha256.as_deref().ok_or_else(|| {
                        LauncherError::ModLoader(format!(
                            "Processor journal has no old hash for {}",
                            entry.target.display()
                        ))
                    })?;
                    move_file_with_cross_volume_fallback(
                        &entry.target,
                        &entry.backup,
                        old_sha256,
                        &self.backup_dir,
                        "processor backup",
                        "target_to_backup",
                        index,
                    )
                    .map_err(|e| {
                        LauncherError::ModLoader(format!(
                            "Failed to preserve {} before publication: {e}",
                            entry.target.display()
                        ))
                    })?;
                }
                // Test-only hook: simulate a staged file changing between the
                // pre-publication verification and the rename.
                test_mutate_staged_output(&entry.staged, index);
                move_file_with_cross_volume_fallback(
                    &entry.staged,
                    &entry.target,
                    &entry.new_sha256,
                    libraries_dir,
                    "processor target",
                    "staged_to_target",
                    index,
                )
                .map_err(|e| {
                    LauncherError::ModLoader(format!(
                        "Failed to publish {}: {e}",
                        entry.target.display()
                    ))
                })?;
                // The staged file was verified above, but the rename is a
                // separate filesystem operation. Re-check the published bytes
                // (and structural/publisher validity) so a changed staging file
                // cannot be committed as a valid mixed generation.
                let published = file_digest(&entry.target)?;
                if published.sha256 != entry.new_sha256 {
                    return Err(LauncherError::ModLoader(format!(
                        "Published output does not match its verified staging hash: {}",
                        entry.target.display()
                    )));
                }
                if !super::forge::is_usable_jar(&entry.target) {
                    return Err(LauncherError::ModLoader(format!(
                        "Published output is not a structurally valid JAR: {}",
                        entry.target.display()
                    )));
                }
                if let Some(publisher_sha1) = &entry.publisher_sha1 {
                    let hash = HashSpec::sha1(publisher_sha1)?;
                    if !hash.verify_file(&entry.target)? {
                        return Err(LauncherError::ModLoader(format!(
                            "Published output failed its publisher checksum after rename: {}",
                            entry.target.display()
                        )));
                    }
                }
                test_failpoint(&format!("after_publish_{index}"));
                if test_error_failpoint(&format!("error_after_publish_{index}")) {
                    return Err(LauncherError::ModLoader(format!(
                        "Injected publication error after {}",
                        entry.target.display()
                    )));
                }
            }
            journal.stage = JournalStage::Committed;
            journal.generation = journal.generation.saturating_add(1);
            self.write_journal(&mut journal)?;
            test_failpoint("after_commit");
            Ok(())
        })();

        if let Err(error) = publish_result {
            if let Err(rollback_error) = self.rollback(&journal, libraries_dir) {
                return Err(LauncherError::ModLoader(format!(
                    "Processor publication failed ({error}); rollback also failed: {rollback_error}"
                )));
            }
            self.cleanup_tx().ok();
            return Err(error);
        }

        self.cleanup_tx()?;
        Ok(())
    }

    fn prepare_journal_entries(
        &self,
        libraries_dir: &Path,
        outputs: &[OutputSpec],
    ) -> Result<Vec<JournalEntry>> {
        let mut entries = Vec::with_capacity(outputs.len());
        for (index, output) in outputs.iter().enumerate() {
            checked_journal_path(libraries_dir, &output.target, "processor target")?;
            checked_journal_path(&self.staging_dir, &output.staged, "processor staging path")?;
            match (&output.publisher_sha1, output.structural_only) {
                (None, true) | (Some(_), false) => {}
                _ => {
                    return Err(LauncherError::ModLoader(format!(
                        "Processor output has inconsistent provenance flags: {}",
                        output.target.display()
                    )))
                }
            }

            let before = file_digest(&output.staged)?;
            if !super::forge::is_usable_jar(&output.staged) {
                return Err(LauncherError::ModLoader(format!(
                    "Processor output is not a structurally valid JAR before publication: {}",
                    output.staged.display()
                )));
            }
            if let Some(publisher_sha1) = &output.publisher_sha1 {
                let hash = HashSpec::sha1(publisher_sha1)?;
                if !hash.verify_file(&output.staged)? {
                    return Err(LauncherError::ModLoader(format!(
                        "Processor output failed its publisher checksum before publication: {}",
                        output.staged.display()
                    )));
                }
            }
            let staged = file_digest(&output.staged)?;
            if before != staged {
                return Err(LauncherError::ModLoader(format!(
                    "Processor staging file changed during verification: {}",
                    output.staged.display()
                )));
            }

            let existed = output.target.exists();
            if existed && !output.target.is_file() {
                return Err(LauncherError::ModLoader(format!(
                    "Processor target is not a regular file: {}",
                    output.target.display()
                )));
            }
            let old_sha256 = if existed {
                Some(file_digest(&output.target)?.sha256)
            } else {
                None
            };
            let backup = self.backup_dir.join(format!("{index}.bak"));
            checked_journal_path(&self.backup_dir, &backup, "processor backup")?;
            if backup.exists() {
                return Err(LauncherError::ModLoader(format!(
                    "Stale transaction backup already exists: {}",
                    backup.display()
                )));
            }
            entries.push(JournalEntry {
                target: output.target.clone(),
                staged: output.staged.clone(),
                backup,
                existed,
                old_sha256,
                new_sha256: staged.sha256,
                publisher_sha1: output.publisher_sha1.clone(),
                structural_only: output.structural_only,
            });
        }
        Ok(entries)
    }

    fn pending_journal_exists(&self) -> bool {
        // `Path::exists` follows links and reports a broken symlink as absent.
        // A journal path that is a directory, symlink or other entry must
        // still block staging and manifest acceptance.
        std::fs::symlink_metadata(&self.journal).is_ok()
            || std::fs::symlink_metadata(&self.journal_previous).is_ok()
    }

    fn load_journal(&self) -> Result<Option<TransactionJournal>> {
        let primary = self.read_journal_file(&self.journal, "primary");
        let previous = self.read_journal_file(&self.journal_previous, "previous copy");
        match (primary, previous) {
            (JournalFile::Missing, JournalFile::Missing) => Ok(None),
            (JournalFile::Valid(journal), JournalFile::Missing)
            | (JournalFile::Missing, JournalFile::Valid(journal)) => Ok(Some(journal)),
            (JournalFile::Valid(newer), JournalFile::Valid(other)) => {
                if newer.generation == other.generation {
                    if newer == other {
                        Ok(Some(newer))
                    } else {
                        Err(journal_recovery_error(
                            "the two journal copies disagree at the same generation",
                        ))
                    }
                } else if newer.generation > other.generation {
                    Ok(Some(newer))
                } else {
                    Ok(Some(other))
                }
            }
            (JournalFile::Valid(journal), JournalFile::Invalid(reason))
            | (JournalFile::Invalid(reason), JournalFile::Valid(journal)) => {
                // One damaged local copy is recoverable from the other. This
                // is resilience against accidental corruption, not against a
                // same-user attacker who can rewrite both copies together.
                tracing::warn!(
                    target: "launcher",
                    "Recovering processor transaction from one valid journal copy; the other is invalid: {}",
                    reason
                );
                Ok(Some(journal))
            }
            (JournalFile::Invalid(primary), JournalFile::Invalid(previous)) => Err(
                journal_recovery_error(format!(
                    "primary: {primary}; previous: {previous}; preserved backups: {}; staging: {}",
                    self.backup_dir.display(),
                    self.staging_dir.display()
                )),
            ),
            (JournalFile::Missing, JournalFile::Invalid(reason))
            | (JournalFile::Invalid(reason), JournalFile::Missing) => Err(journal_recovery_error(
                format!(
                    "one journal copy is missing and the other is invalid: {reason}; preserved backups: {}; staging: {}",
                    self.backup_dir.display(),
                    self.staging_dir.display()
                ),
            )),
        }
    }

    fn read_journal_file(&self, path: &Path, label: &str) -> JournalFile {
        match read_bounded_json(path) {
            Ok(None) => JournalFile::Missing,
            Ok(Some(bytes)) => match serde_json::from_slice::<TransactionJournal>(&bytes) {
                Ok(journal) => {
                    if let Err(e) = validate_journal_structure(&journal) {
                        JournalFile::Invalid(format!(
                            "{label} journal at {} is invalid: {e}",
                            path.display()
                        ))
                    } else if let Err(e) = journal.verify_seal() {
                        JournalFile::Invalid(format!(
                            "{label} journal at {} failed its integrity check: {e}",
                            path.display()
                        ))
                    } else {
                        JournalFile::Valid(journal)
                    }
                }
                Err(e) => JournalFile::Invalid(format!(
                    "{label} journal at {} is malformed: {e}",
                    path.display()
                )),
            },
            Err(e) => JournalFile::Invalid(format!(
                "{label} journal at {} is unreadable: {e}",
                path.display()
            )),
        }
    }

    fn write_journal(&self, journal: &mut TransactionJournal) -> Result<()> {
        if journal.generation == 0 {
            journal.generation = 1;
        }
        journal.seal()?;
        validate_journal_structure(journal)?;
        let generation = journal.generation;
        if generation > 1 {
            // Never remove the last valid journal before verifying that the
            // authoritative copy we are about to advance still exists.
            match self.read_journal_file(&self.journal_previous, "authoritative") {
                JournalFile::Valid(existing)
                    if existing.tx_id == journal.tx_id
                        && existing.generation < generation => {}
                _ => {
                    return Err(LauncherError::ModLoader(format!(
                        "Refusing to advance processor journal generation {}: the authoritative secondary copy is missing or invalid",
                        generation
                    )))
                }
            }
        }
        let bytes = serde_json::to_vec_pretty(journal).map_err(|e| {
            LauncherError::ModLoader(format!("Failed to serialize processor journal: {e}"))
        })?;
        // Current protocol:
        //   1. the authoritative copy is the secondary journal;
        //   2. invalidate the stale primary mirror while the authoritative
        //      copy is untouched; abort without advancing if this fails;
        //   3. write the new authoritative generation;
        //   4. write a new primary mirror (best effort; the authoritative copy
        //      remains valid if the mirror fails).
        //
        // This ordering guarantees that an older valid primary is never left
        // behind while a newer committed secondary exists. If the secondary is
        // later damaged, recovery fails closed instead of selecting the old
        // primary.
        test_failpoint(&format!("before_mirror_invalidation_{generation}"));
        self.invalidate_journal_mirror(generation)?;
        test_failpoint(&format!("after_mirror_invalidation_{generation}"));
        self.write_journal_copy(&self.journal_previous, &bytes, "secondary", generation)?;
        test_failpoint(match &journal.stage {
            JournalStage::Prepared => "after_secondary_prepared_journal",
            JournalStage::Committed => "after_secondary_committed_journal",
        });
        if let Err(error) = self.write_journal_copy(&self.journal, &bytes, "primary", generation) {
            tracing::warn!(
                target: "launcher",
                "Primary processor journal mirror failed after the authoritative secondary copy was committed at generation {}: {}",
                generation,
                error
            );
        }
        Ok(())
    }

    fn invalidate_journal_mirror(&self, generation: u64) -> Result<()> {
        match std::fs::remove_file(&self.journal) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(LauncherError::ModLoader(format!(
                "Cannot invalidate the stale primary processor journal at {}; transaction generation {} was not advanced: {error}",
                self.journal.display(),
                generation
            ))),
        }
    }

    fn write_journal_copy(
        &self,
        path: &Path,
        bytes: &[u8],
        copy: &str,
        generation: u64,
    ) -> Result<()> {
        let parent = path.parent().ok_or_else(|| {
            LauncherError::ModLoader(format!(
                "Processor journal path has no parent directory: {}",
                path.display()
            ))
        })?;
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                LauncherError::ModLoader(format!(
                    "Processor journal path has no valid file name: {}",
                    path.display()
                ))
            })?;
        let temp = parent.join(format!(".{file_name}.{}.tmp", Uuid::new_v4()));
        let _ = std::fs::remove_file(&temp);
        journal_write_failpoint(generation, copy, "write")?;
        if let Err(error) = std::fs::write(&temp, bytes) {
            let _ = std::fs::remove_file(&temp);
            return Err(error.into());
        }
        journal_temp_corrupt_failpoint(generation, copy, &temp)?;
        journal_write_failpoint(generation, copy, "sync")?;
        if let Err(error) = std::fs::OpenOptions::new()
            .write(true)
            .open(&temp)
            .and_then(|file| file.sync_all())
        {
            let _ = std::fs::remove_file(&temp);
            return Err(LauncherError::ModLoader(format!(
                "Failed to flush processor journal {copy} copy: {error}"
            )));
        }
        journal_write_failpoint(generation, copy, "rename")?;
        if let Err(error) = std::fs::rename(&temp, path) {
            let _ = std::fs::remove_file(&temp);
            return Err(error.into());
        }
        Ok(())
    }

    fn validate_journal_paths(
        &self,
        journal: &TransactionJournal,
        libraries_dir: &Path,
    ) -> Result<()> {
        validate_journal_structure(journal)?;
        // The transaction directories are part of the containment boundary.
        // Checking only paths below them would accept a junction used *as*
        // staging/backup/tx.
        crate::import::ensure_no_reparse_points(&self.data_dir, &self.root)?;
        crate::import::ensure_no_reparse_points(&self.root, &self.tx_dir)?;
        crate::import::ensure_no_reparse_points(&self.tx_dir, &self.staging_dir)?;
        crate::import::ensure_no_reparse_points(&self.tx_dir, &self.backup_dir)?;
        for entry in &journal.entries {
            checked_journal_path(libraries_dir, &entry.target, "target")?;
            checked_journal_path(&self.staging_dir, &entry.staged, "staged output")?;
            checked_journal_path(&self.backup_dir, &entry.backup, "backup")?;
        }
        Ok(())
    }

    fn rollback(&self, journal: &TransactionJournal, libraries_dir: &Path) -> Result<()> {
        self.validate_journal_paths(journal, libraries_dir)?;
        // Preflight every entry before changing the first target. If a backup
        // needed by a later entry is missing/corrupt, restoring an earlier
        // entry would leave a mixed old/new set.
        self.verify_rollback_ready(journal, libraries_dir)?;
        for (index, entry) in journal.entries.iter().enumerate().rev() {
            self.rollback_entry(entry, libraries_dir, index)?;
            test_failpoint("rollback_after_first_restore");
        }
        Ok(())
    }

    fn verify_rollback_ready(
        &self,
        journal: &TransactionJournal,
        libraries_dir: &Path,
    ) -> Result<()> {
        for entry in &journal.entries {
            match (entry.existed, entry.old_sha256.as_deref()) {
                (true, Some(old_sha256)) => {
                    if entry.target.exists() && !entry.target.is_file() {
                        return Err(LauncherError::ModLoader(format!(
                            "Cannot roll back processor transaction: target is not a regular file at {}; no targets were changed",
                            entry.target.display()
                        )));
                    }
                    let target_is_old =
                        entry.target.is_file() && file_digest(&entry.target)?.sha256 == old_sha256;
                    if target_is_old {
                        continue;
                    }
                    if !entry.backup.is_file() {
                        return Err(LauncherError::ModLoader(format!(
                            "Cannot roll back processor transaction: backup is missing for {}; no targets were changed",
                            entry.target.display()
                        )));
                    }
                    if file_digest(&entry.backup)?.sha256 != old_sha256 {
                        return Err(LauncherError::ModLoader(format!(
                            "Cannot roll back processor transaction: backup is corrupt for {}; no targets were changed",
                            entry.target.display()
                        )));
                    }
                    // Fail before restoring any target if the destination
                    // directory cannot be created/prepared.
                    prepare_target_parent(libraries_dir, &entry.target)?;
                }
                (false, None) => {
                    if entry.target.exists() {
                        let untouched_new = entry.target.is_file()
                            && file_digest(&entry.target)?.sha256 == entry.new_sha256;
                        if !untouched_new {
                            return Err(LauncherError::ModLoader(format!(
                                "Cannot roll back processor transaction: newly created output changed at {}; no targets were changed",
                                entry.target.display()
                            )));
                        }
                    }
                }
                _ => {
                    return Err(LauncherError::ModLoader(format!(
                        "Processor journal has inconsistent old-output state for {}",
                        entry.target.display()
                    )))
                }
            }
        }
        Ok(())
    }

    fn rollback_entry(
        &self,
        entry: &JournalEntry,
        libraries_dir: &Path,
        index: usize,
    ) -> Result<()> {
        let old_sha256 = match (entry.existed, entry.old_sha256.as_deref()) {
            (true, Some(value)) => Some(value),
            (false, None) => None,
            _ => {
                return Err(LauncherError::ModLoader(format!(
                    "Processor journal has inconsistent old-output state for {}",
                    entry.target.display()
                )))
            }
        };

        if let Some(old_sha256) = old_sha256 {
            let target_is_old =
                entry.target.is_file() && file_digest(&entry.target)?.sha256 == old_sha256;
            let backup_exists = entry.backup.is_file();
            if target_is_old {
                // A previous rollback (or a crash after the replace) already
                // restored the old file. The backup is now redundant, even if
                // it was damaged; the verified target is authoritative.
                if backup_exists {
                    std::fs::remove_file(&entry.backup).map_err(|e| {
                        LauncherError::ModLoader(format!(
                            "Failed to remove a redundant backup {}: {e}",
                            entry.backup.display()
                        ))
                    })?;
                }
            } else if backup_exists {
                let backup_digest = file_digest(&entry.backup)?;
                if backup_digest.sha256 != old_sha256 {
                    return Err(LauncherError::ModLoader(format!(
                        "Previous-output backup is corrupt: {}",
                        entry.backup.display()
                    )));
                }
                // Copy, do not move: the backup must remain available until the
                // restored target has been verified. A crash or hash mismatch
                // after the replace can then retry from the same backup.
                prepare_target_parent(libraries_dir, &entry.target)?;
                let temp = replacement_temp_path(&entry.target)?;
                checked_journal_path(libraries_dir, &temp, "rollback temporary file")?;
                let _ = std::fs::remove_file(&temp);
                std::fs::copy(&entry.backup, &temp).map_err(|e| {
                    LauncherError::ModLoader(format!(
                        "Failed to prepare previous output {} from its backup: {e}",
                        entry.target.display()
                    ))
                })?;
                if let Ok(file) = std::fs::OpenOptions::new().write(true).open(&temp) {
                    let _ = file.sync_all();
                }
                let temp_digest = file_digest(&temp)?;
                if temp_digest.sha256 != old_sha256 {
                    let _ = std::fs::remove_file(&temp);
                    return Err(LauncherError::ModLoader(format!(
                        "Prepared previous-output copy is corrupt: {}",
                        temp.display()
                    )));
                }
                rename_with_failpoint(&temp, &entry.target, "backup_to_target", index).map_err(
                    |e| {
                        let _ = std::fs::remove_file(&temp);
                        LauncherError::ModLoader(format!(
                            "Failed to restore previous output {}: {e}",
                            entry.target.display()
                        ))
                    },
                )?;
                test_corrupt_restored_target(&entry.target);
                let restored = file_digest(&entry.target)?;
                if restored.sha256 != old_sha256 {
                    // Keep the backup: a later recovery can retry. Removing it
                    // here would destroy the last recoverable old file.
                    return Err(LauncherError::ModLoader(format!(
                        "Restored output does not match its previous hash: {}; backup preserved at {}",
                        entry.target.display(),
                        entry.backup.display()
                    )));
                }
                std::fs::remove_file(&entry.backup).map_err(|e| {
                    LauncherError::ModLoader(format!(
                        "Restored output {} but could not remove its backup {}: {e}",
                        entry.target.display(),
                        entry.backup.display()
                    ))
                })?;
            } else {
                return Err(LauncherError::ModLoader(format!(
                    "Cannot restore previous output {}: backup is missing",
                    entry.target.display()
                )));
            }
        } else {
            if entry.backup.exists() {
                return Err(LauncherError::ModLoader(format!(
                    "Unexpected backup for a newly created processor output: {}",
                    entry.backup.display()
                )));
            }
            if entry.target.exists() {
                let current = file_digest(&entry.target)?.sha256;
                if current != entry.new_sha256 {
                    return Err(LauncherError::ModLoader(format!(
                        "Cannot remove a changed untracked output during rollback: {}",
                        entry.target.display()
                    )));
                }
                std::fs::remove_file(&entry.target).map_err(|e| {
                    LauncherError::ModLoader(format!(
                        "Failed to remove newly published output {}: {e}",
                        entry.target.display()
                    ))
                })?;
            }
        }

        if entry.staged.exists() {
            if entry.staged.is_file() {
                std::fs::remove_file(&entry.staged).ok();
            } else {
                return Err(LauncherError::ModLoader(format!(
                    "Staged processor path is not a regular file during rollback: {}",
                    entry.staged.display()
                )));
            }
        }
        Ok(())
    }

    fn cleanup_tx(&self) -> Result<()> {
        cleanup_failpoint("staging")?;
        if self.staging_dir.exists() {
            crate::import::ensure_no_reparse_points(&self.tx_dir, &self.staging_dir)?;
            std::fs::remove_dir_all(&self.staging_dir)?;
        }
        cleanup_failpoint("backup")?;
        if self.backup_dir.exists() {
            crate::import::ensure_no_reparse_points(&self.tx_dir, &self.backup_dir)?;
            let mut removed_any = false;
            for entry in std::fs::read_dir(&self.backup_dir)? {
                let path = entry?.path();
                let metadata = std::fs::symlink_metadata(&path)?;
                if metadata.is_dir() {
                    std::fs::remove_dir_all(&path)?;
                } else {
                    std::fs::remove_file(&path)?;
                }
                if !removed_any {
                    removed_any = true;
                    // Test-only crash point: leaves a committed journal with
                    // one backup already deleted, exercising Recovery Required.
                    test_failpoint("cleanup_after_first_backup_removed");
                }
            }
            std::fs::remove_dir(&self.backup_dir)?;
        }
        // Remove the journals last: if any earlier cleanup step fails, the
        // next launch still sees the transaction and retries recovery. The
        // primary is removed before the previous copy so a failed removal
        // cannot leave only the secondary as evidence.
        cleanup_failpoint("journal")?;
        if self.journal.exists() {
            std::fs::remove_file(&self.journal)?;
        }
        cleanup_failpoint("journal_previous")?;
        if self.journal_previous.exists() {
            std::fs::remove_file(&self.journal_previous)?;
        }
        Ok(())
    }
}

enum JournalFile {
    Missing,
    Valid(TransactionJournal),
    Invalid(String),
}

fn journal_recovery_error(detail: impl std::fmt::Display) -> LauncherError {
    LauncherError::ModLoader(format!(
        "Processor transaction journal is corrupt or unreadable ({detail}). \
         Backups and staging were preserved; the instance cannot launch until the transaction is repaired."
    ))
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn is_sha1_hex(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Validate every semantic invariant that must hold before a journal is
/// allowed to drive filesystem operations. This does not check containment;
/// `validate_journal_paths` does that with the actual cache directories.
fn validate_journal_structure(journal: &TransactionJournal) -> Result<()> {
    if journal.schema_version != JOURNAL_SCHEMA_VERSION {
        return Err(LauncherError::ModLoader(format!(
            "Unsupported processor transaction journal version: {}",
            journal.schema_version
        )));
    }
    if journal.generation == 0 {
        return Err(LauncherError::ModLoader(
            "Processor transaction journal has no generation".into(),
        ));
    }
    if Uuid::parse_str(&journal.tx_id).is_err() {
        return Err(LauncherError::ModLoader(
            "Processor transaction journal has an invalid transaction id".into(),
        ));
    }
    if journal.entries.is_empty() {
        return Err(LauncherError::ModLoader(
            "Processor transaction journal has no outputs".into(),
        ));
    }
    if journal.entries.len() > MAX_JOURNAL_ENTRIES {
        return Err(LauncherError::ModLoader(format!(
            "Processor transaction journal has too many entries: {}",
            journal.entries.len()
        )));
    }

    let mut identities = HashSet::new();
    for entry in &journal.entries {
        if !is_sha256_hex(&entry.new_sha256) {
            return Err(LauncherError::ModLoader(format!(
                "Processor transaction journal has a malformed new SHA-256 for {}",
                entry.target.display()
            )));
        }
        match (entry.existed, entry.old_sha256.as_deref()) {
            (true, Some(value)) if is_sha256_hex(value) => {}
            (true, Some(_)) => {
                return Err(LauncherError::ModLoader(format!(
                    "Processor transaction journal has a malformed old SHA-256 for {}",
                    entry.target.display()
                )))
            }
            (true, None) => {
                return Err(LauncherError::ModLoader(format!(
                    "Processor transaction journal has an existing target without an old hash: {}",
                    entry.target.display()
                )))
            }
            (false, None) => {}
            (false, Some(_)) => {
                return Err(LauncherError::ModLoader(format!(
                    "Processor transaction journal has an old hash for a newly created target: {}",
                    entry.target.display()
                )))
            }
        }
        if let Some(publisher_sha1) = &entry.publisher_sha1 {
            if !is_sha1_hex(publisher_sha1) {
                return Err(LauncherError::ModLoader(format!(
                    "Processor transaction journal has a malformed publisher SHA-1 for {}",
                    entry.target.display()
                )));
            }
        }
        if entry.structural_only != entry.publisher_sha1.is_none() {
            return Err(LauncherError::ModLoader(format!(
                "Processor transaction journal has inconsistent provenance flags for {}",
                entry.target.display()
            )));
        }
        for (label, path) in [
            ("target", &entry.target),
            ("staged", &entry.staged),
            ("backup", &entry.backup),
        ] {
            if !identities.insert(path_identity(path)) {
                return Err(LauncherError::ModLoader(format!(
                    "Processor transaction journal has a duplicate or overlapping {label} path: {}",
                    path.display()
                )));
            }
        }
    }
    Ok(())
}

/// Reject a journal path unless it is a purely relative path below `base` with
/// no traversal, root, drive/UNC prefix or alternate data stream component.
/// `Path::starts_with` alone is not sufficient: `libraries/../outside.jar`
/// is lexically below `libraries` before the parent component is resolved.
fn checked_journal_path(base: &Path, path: &Path, label: &str) -> Result<()> {
    let relative = path.strip_prefix(base).map_err(|_| {
        LauncherError::ModLoader(format!(
            "Processor transaction {label} path is outside {}: {}",
            base.display(),
            path.display()
        ))
    })?;
    if relative.as_os_str().is_empty() {
        return Err(LauncherError::ModLoader(format!(
            "Processor transaction {label} path names the base directory itself: {}",
            path.display()
        )));
    }
    let text = relative.to_str().ok_or_else(|| {
        LauncherError::ModLoader(format!(
            "Processor transaction {label} path is not valid UTF-8: {}",
            path.display()
        ))
    })?;
    crate::import::check_safe_relative(text).map_err(|e| {
        LauncherError::ModLoader(format!(
            "Unsafe processor transaction {label} path {}: {e}",
            path.display()
        ))
    })?;
    for component in relative.components() {
        match component {
            Component::Normal(value) => {
                let text = value.to_string_lossy();
                // Win32 strips trailing dots/spaces, so `foo.` and `foo `
                // alias another file. Reject aliases rather than let the
                // journal address a path different from the one recorded.
                if text.is_empty() || text.trim_end_matches(['.', ' ']) != text {
                    return Err(LauncherError::ModLoader(format!(
                        "Processor transaction {label} path contains an unsafe component: {}",
                        path.display()
                    )));
                }
            }
            _ => {
                return Err(LauncherError::ModLoader(format!(
                    "Processor transaction {label} path contains a non-normal component: {}",
                    path.display()
                )))
            }
        }
    }
    crate::import::ensure_no_reparse_points(base, path)?;
    Ok(())
}

/// Ensure the target's parent directory exists and stays inside
/// `libraries_dir` without traversing a reparse point. This is needed before a
/// cross-volume copy because the temporary replacement must live on the
/// target's volume.
fn prepare_target_parent(libraries_dir: &Path, target: &Path) -> Result<()> {
    let parent = target.parent().ok_or_else(|| {
        LauncherError::ModLoader(format!(
            "Processor target has no parent directory: {}",
            target.display()
        ))
    })?;
    if parent != libraries_dir {
        checked_journal_path(libraries_dir, parent, "processor target directory")?;
    }
    std::fs::create_dir_all(parent)?;
    crate::import::ensure_no_reparse_points(libraries_dir, parent)?;
    Ok(())
}

/// Strict bounded read used for the transaction journal. A missing file is
/// `None`; an oversized, unreadable, non-regular or symlinked file is an
/// error. Treating those as "no transaction" would let a corrupt journal be
/// ignored and a mixed output set be accepted.
fn read_bounded_json(path: &Path) -> Result<Option<Vec<u8>>> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if metadata.file_type().is_symlink() {
        return Err(LauncherError::ModLoader(format!(
            "Processor journal is a symbolic link: {}",
            path.display()
        )));
    }
    if !metadata.is_file() {
        return Err(LauncherError::ModLoader(format!(
            "Processor journal is not a regular file: {}",
            path.display()
        )));
    }
    if metadata.len() > MAX_JSON_BYTES {
        return Err(LauncherError::ModLoader(format!(
            "Processor journal exceeds the {} byte limit: {}",
            MAX_JSON_BYTES,
            path.display()
        )));
    }
    let bytes = std::fs::read(path)?;
    if bytes.len() as u64 > MAX_JSON_BYTES {
        return Err(LauncherError::ModLoader(format!(
            "Processor journal grew beyond the {} byte limit while being read: {}",
            MAX_JSON_BYTES,
            path.display()
        )));
    }
    Ok(Some(bytes))
}

fn replacement_temp_path(destination: &Path) -> Result<PathBuf> {
    let parent = destination.parent().ok_or_else(|| {
        LauncherError::ModLoader(format!(
            "Processor destination has no parent directory: {}",
            destination.display()
        ))
    })?;
    let name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            LauncherError::ModLoader(format!(
                "Processor destination has no valid file name: {}",
                destination.display()
            ))
        })?;
    Ok(parent.join(format!(".{name}.rollback-{}.tmp", Uuid::new_v4())))
}

impl InstallerIdentity {
    fn into_verification(self) -> Result<super::artifact::InstallerVerification> {
        let checksum = HashSpec::parse(&self.checksum_algorithm, &self.checksum_hex)?;
        Ok(super::artifact::InstallerVerification {
            checksum,
            source: self.checksum_source,
            url: self.url,
        })
    }
}

pub(crate) fn path_identity(path: &Path) -> String {
    // Normalize components rather than the raw string so `a/./b`, `a//b`
    // and `a\b` compare as the same path on Windows. The result is used only
    // for duplicate detection, never as a filesystem path.
    let mut parts = Vec::new();
    for component in path.components() {
        parts.push(match component {
            Component::Prefix(prefix) => prefix.as_os_str().to_string_lossy().to_string(),
            Component::RootDir => "/".to_string(),
            Component::CurDir => ".".to_string(),
            Component::ParentDir => "..".to_string(),
            Component::Normal(value) => value.to_string_lossy().to_string(),
        });
    }
    let value = parts.join("/");
    if cfg!(windows) {
        value.to_lowercase()
    } else {
        value
    }
}

pub(crate) fn relative_identity(value: &str) -> String {
    let normalized = value.replace('\\', "/");
    if cfg!(windows) {
        normalized.to_lowercase()
    } else {
        normalized
    }
}

fn stale(what: &str) -> LauncherError {
    LauncherError::ModLoader(format!(
        "Cached processor provenance is stale or invalid: {what}"
    ))
}

fn read_small_json(path: &Path) -> Result<Option<Vec<u8>>> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if !metadata.is_file() || metadata.len() > MAX_JSON_BYTES {
        return Ok(None);
    }
    Ok(Some(std::fs::read(path)?))
}

pub(crate) fn file_digest(path: &Path) -> Result<FileDigest> {
    let metadata = std::fs::metadata(path)?;
    if !metadata.is_file() {
        return Err(LauncherError::ModLoader(format!(
            "Expected a regular file: {}",
            path.display()
        )));
    }
    Ok(FileDigest {
        sha256: crate::download::hash_file_sha256(path)?,
        size: metadata.len(),
    })
}

pub(crate) fn sha256_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub(crate) fn checked_relative(base: &Path, relative: &str) -> Result<PathBuf> {
    crate::import::check_safe_remote_path(relative)
        .map_err(|e| LauncherError::ModLoader(format!("Unsafe cache path {relative:?}: {e}")))?;
    let path = base.join(relative);
    if !path.starts_with(base) || path == base {
        return Err(LauncherError::ModLoader(format!(
            "Cache path escapes its base: {relative}"
        )));
    }
    crate::import::ensure_no_reparse_points(base, &path)?;
    Ok(path)
}

pub(crate) fn relative_to(base: &Path, path: &Path) -> Result<String> {
    let relative = path.strip_prefix(base).map_err(|_| {
        LauncherError::ModLoader(format!(
            "Path {} is outside {}",
            path.display(),
            base.display()
        ))
    })?;
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(value) => parts.push(value.to_string_lossy().to_string()),
            _ => {
                return Err(LauncherError::ModLoader(format!(
                    "Unsafe relative path component in {}",
                    path.display()
                )))
            }
        }
    }
    let value = parts.join("/");
    crate::import::check_safe_remote_path(&value).map_err(|e| {
        LauncherError::ModLoader(format!("Unsafe relative path {}: {e}", path.display()))
    })?;
    Ok(value)
}

fn test_failpoint(name: &str) {
    #[cfg(test)]
    {
        if std::env::var("VOIDLAUNCHER_TX_FAILPOINT").ok().as_deref() == Some(name) {
            std::process::abort();
        }
    }
    #[cfg(not(test))]
    {
        let _ = name;
    }
}

#[cfg(test)]
static TEST_ERROR_AFTER_PUBLISH: std::sync::atomic::AtomicIsize =
    std::sync::atomic::AtomicIsize::new(-1);

#[cfg(test)]
pub(crate) fn set_test_error_after_publish(index: isize) {
    TEST_ERROR_AFTER_PUBLISH.store(index, std::sync::atomic::Ordering::SeqCst);
}

fn test_error_failpoint(name: &str) -> bool {
    #[cfg(test)]
    {
        if let Some(index) = name
            .strip_prefix("error_after_publish_")
            .and_then(|value| value.parse::<isize>().ok())
        {
            if index == TEST_ERROR_AFTER_PUBLISH.load(std::sync::atomic::Ordering::SeqCst) {
                return true;
            }
        }
        std::env::var("VOIDLAUNCHER_TX_ERROR").ok().as_deref() == Some(name)
    }
    #[cfg(not(test))]
    {
        let _ = name;
        false
    }
}

#[cfg(test)]
static TEST_RENAME_FAILURE: StdMutex<Option<(String, isize)>> = StdMutex::new(None);

#[cfg(test)]
static TEST_CROSS_DEVICE_RENAME: StdMutex<Option<(String, isize)>> = StdMutex::new(None);

#[cfg(test)]
pub(crate) fn set_test_rename_failure(stage: Option<&str>, index: isize) {
    *TEST_RENAME_FAILURE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) =
        stage.map(|stage| (stage.to_string(), index));
}

#[cfg(all(test, windows))]
pub(crate) fn set_test_cross_device_rename(stage: Option<&str>, index: isize) {
    *TEST_CROSS_DEVICE_RENAME
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) =
        stage.map(|stage| (stage.to_string(), index));
}

fn rename_with_failpoint(from: &Path, to: &Path, stage: &str, index: usize) -> std::io::Result<()> {
    #[cfg(test)]
    {
        let guard = TEST_RENAME_FAILURE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if guard
            .as_ref()
            .is_some_and(|(name, target_index)| name == stage && *target_index == index as isize)
        {
            return Err(std::io::Error::other(format!(
                "injected rename failure: {stage}[{index}]"
            )));
        }
        let cross_device = TEST_CROSS_DEVICE_RENAME
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if cross_device
            .as_ref()
            .is_some_and(|(name, target_index)| name == stage && *target_index == index as isize)
        {
            // Windows ERROR_NOT_SAME_DEVICE. The caller must use the
            // copy-to-local-temp fallback rather than surfacing this error.
            return Err(std::io::Error::from_raw_os_error(17));
        }
    }
    let _ = (stage, index);
    std::fs::rename(from, to)
}

fn is_cross_device_error(error: &std::io::Error) -> bool {
    #[cfg(windows)]
    {
        // ERROR_NOT_SAME_DEVICE.
        error.raw_os_error() == Some(17)
    }
    #[cfg(not(windows))]
    {
        error.raw_os_error() == Some(18) // EXDEV
    }
}

/// Move a transaction file, falling back to a copy + same-volume atomic
/// replace when Windows refuses a rename across volumes.
///
/// `libraries_dir` may be a junction to another volume. A rename from that
/// target volume to `tx/backup` (or from `tx/staging` to the target volume)
/// then fails with `ERROR_NOT_SAME_DEVICE`. The fallback copies into the
/// destination directory first and renames only within that directory, so
/// the final replacement stays atomic on the destination volume.
fn move_file_with_cross_volume_fallback(
    source: &Path,
    destination: &Path,
    expected_sha256: &str,
    destination_base: &Path,
    label: &str,
    stage: &str,
    index: usize,
) -> Result<()> {
    match rename_with_failpoint(source, destination, stage, index) {
        Ok(()) => Ok(()),
        Err(error) if is_cross_device_error(&error) => {
            let temp_destination = replacement_temp_path(destination)?;
            checked_journal_path(destination_base, &temp_destination, label)?;
            let _ = std::fs::remove_file(&temp_destination);
            if let Err(error) = std::fs::copy(source, &temp_destination) {
                let _ = std::fs::remove_file(&temp_destination);
                return Err(LauncherError::ModLoader(format!(
                    "Failed to copy {label} across volumes ({} -> {}): {error}",
                    source.display(),
                    destination.display()
                )));
            }
            match std::fs::OpenOptions::new()
                .write(true)
                .open(&temp_destination)
                .and_then(|file| file.sync_all())
            {
                Ok(()) => {}
                Err(error) => {
                    let _ = std::fs::remove_file(&temp_destination);
                    return Err(LauncherError::ModLoader(format!(
                        "Failed to sync the cross-volume copy of {label}: {error}"
                    )));
                }
            }
            let temp_digest = file_digest(&temp_destination)?;
            if temp_digest.sha256 != expected_sha256 {
                let _ = std::fs::remove_file(&temp_destination);
                return Err(LauncherError::ModLoader(format!(
                    "Cross-volume copy of {label} does not match its verified hash: {}",
                    temp_destination.display()
                )));
            }
            if let Err(error) = std::fs::rename(&temp_destination, destination) {
                let _ = std::fs::remove_file(&temp_destination);
                return Err(LauncherError::ModLoader(format!(
                    "Failed to replace {label} from its cross-volume copy: {error}"
                )));
            }
            let destination_digest = file_digest(destination)?;
            if destination_digest.sha256 != expected_sha256 {
                // The destination was created by this copy and the source is
                // still intact. Remove the bad copy so rollback is not blocked
                // by a corrupt backup/target.
                let _ = std::fs::remove_file(destination);
                return Err(LauncherError::ModLoader(format!(
                    "Cross-volume {label} replacement does not match its verified hash: {}",
                    destination.display()
                )));
            }
            if let Err(error) = std::fs::remove_file(source) {
                return Err(LauncherError::ModLoader(format!(
                    "Cross-volume {label} was copied to {} but its source could not be removed: {error}",
                    destination.display()
                )));
            }
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
static TEST_MUTATE_STAGED_OUTPUT: std::sync::atomic::AtomicIsize =
    std::sync::atomic::AtomicIsize::new(-1);

#[cfg(test)]
pub(crate) fn set_test_mutate_staged_output(index: isize) {
    TEST_MUTATE_STAGED_OUTPUT.store(index, std::sync::atomic::Ordering::SeqCst);
}

fn test_mutate_staged_output(path: &Path, index: usize) {
    #[cfg(test)]
    {
        if TEST_MUTATE_STAGED_OUTPUT.load(std::sync::atomic::Ordering::SeqCst) == index as isize {
            use std::io::Write as _;
            if let Ok(mut file) = std::fs::OpenOptions::new().append(true).open(path) {
                let _ = file.write_all(b"tampered-after-verification");
            }
        }
    }
    #[cfg(not(test))]
    {
        let _ = (path, index);
    }
}

#[cfg(test)]
static TEST_CORRUPT_RESTORED_TARGET: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[cfg(test)]
pub(crate) fn set_test_corrupt_restored_target(enabled: bool) {
    TEST_CORRUPT_RESTORED_TARGET.store(enabled, std::sync::atomic::Ordering::SeqCst);
}

fn test_corrupt_restored_target(path: &Path) {
    #[cfg(test)]
    {
        if TEST_CORRUPT_RESTORED_TARGET.load(std::sync::atomic::Ordering::SeqCst) {
            let _ = std::fs::write(path, b"injected-restore-mismatch");
        }
    }
    #[cfg(not(test))]
    {
        let _ = path;
    }
}

#[cfg(test)]
static TEST_CLEANUP_FAILURE: StdMutex<Option<String>> = StdMutex::new(None);

#[cfg(test)]
pub(crate) fn set_test_cleanup_failure(stage: Option<&str>) {
    *TEST_CLEANUP_FAILURE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = stage.map(str::to_string);
}

fn cleanup_failpoint(stage: &str) -> std::io::Result<()> {
    #[cfg(test)]
    {
        let guard = TEST_CLEANUP_FAILURE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if guard.as_deref() == Some(stage) {
            return Err(std::io::Error::other(format!(
                "injected cleanup failure: {stage}"
            )));
        }
    }
    let _ = stage;
    Ok(())
}

#[cfg(test)]
static TEST_JOURNAL_WRITE_FAILURE: StdMutex<Option<(u64, String, String)>> = StdMutex::new(None);

#[cfg(test)]
pub(crate) fn set_test_journal_write_failure(failure: Option<(u64, &str, &str)>) {
    *TEST_JOURNAL_WRITE_FAILURE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) =
        failure.map(|(generation, copy, stage)| (generation, copy.to_string(), stage.to_string()));
}

fn journal_write_failpoint(generation: u64, copy: &str, stage: &str) -> std::io::Result<()> {
    #[cfg(test)]
    {
        let guard = TEST_JOURNAL_WRITE_FAILURE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if guard
            .as_ref()
            .is_some_and(|(expected_generation, expected_copy, expected_stage)| {
                *expected_generation == generation
                    && expected_copy == copy
                    && expected_stage == stage
            })
        {
            return Err(std::io::Error::other(format!(
                "injected journal write failure: {copy}/{stage} at generation {generation}"
            )));
        }
    }
    let _ = (generation, copy, stage);
    Ok(())
}

#[cfg(test)]
static TEST_JOURNAL_TEMP_CORRUPT: StdMutex<Option<(u64, String)>> = StdMutex::new(None);

#[cfg(test)]
pub(crate) fn set_test_journal_temp_corrupt(failure: Option<(u64, &str)>) {
    *TEST_JOURNAL_TEMP_CORRUPT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) =
        failure.map(|(generation, copy)| (generation, copy.to_string()));
}

fn journal_temp_corrupt_failpoint(generation: u64, copy: &str, temp: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    {
        let guard = TEST_JOURNAL_TEMP_CORRUPT
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if guard
            .as_ref()
            .is_some_and(|(expected_generation, expected_copy)| {
                *expected_generation == generation && expected_copy == copy
            })
        {
            std::fs::write(temp, b"{corrupt-temp-journal")?;
        }
    }
    let _ = (generation, copy, temp);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::download::tests::{direct_proxy_lock, sha1_hex, sha512_hex, spawn_mock, MockSpec};
    use std::io::Write;
    use std::sync::Mutex;

    static TX_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn test_root(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "voidlauncher_processor_cache_{}_{}",
            std::process::id(),
            tag
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn test_cache(root: &Path) -> ProcessorCache {
        ProcessorCache::new(root, "forge", "1.20.1", "1.20.1-47.4.29").unwrap()
    }

    fn write_file(path: &Path, bytes: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut file = std::fs::File::create(path).unwrap();
        file.write_all(bytes).unwrap();
        file.sync_all().unwrap();
    }

    fn test_jar(label: &str) -> Vec<u8> {
        let mut buffer = std::io::Cursor::new(Vec::new());
        {
            let mut archive = zip::ZipWriter::new(&mut buffer);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored)
                .last_modified_time(zip::DateTime::default());
            archive.start_file("META-INF/MANIFEST.MF", options).unwrap();
            let mut manifest =
                format!("Manifest-Version: 1.0\nX-VoidLauncher-Test: {label}\n").into_bytes();
            // `is_usable_jar` rejects files below 1 KiB.
            manifest.resize(1100, b' ');
            archive.write_all(&manifest).unwrap();
            archive.finish().unwrap();
        }
        buffer.into_inner()
    }

    fn journal_entry(target: PathBuf, staged: PathBuf, backup: PathBuf) -> JournalEntry {
        JournalEntry {
            target,
            staged,
            backup,
            existed: false,
            old_sha256: None,
            new_sha256: "a".repeat(64),
            publisher_sha1: None,
            structural_only: true,
        }
    }

    fn test_journal(entries: Vec<JournalEntry>) -> TransactionJournal {
        let mut journal = TransactionJournal::new(entries);
        journal.generation = 1;
        journal.seal().unwrap();
        journal
    }

    fn write_journal_files(cache: &ProcessorCache, journal: &TransactionJournal) {
        let bytes = serde_json::to_vec_pretty(journal).unwrap();
        write_file(&cache.journal, &bytes);
        write_file(&cache.journal_previous, &bytes);
    }

    fn assert_recovery_rejects(cache: &ProcessorCache, libs: &Path, journal: &TransactionJournal) {
        write_journal_files(cache, journal);
        let error = cache.recover_pending_transaction(libs).unwrap_err();
        assert!(
            error.to_string().contains("journal"),
            "unexpected recovery error: {error}"
        );
    }

    #[cfg(windows)]
    fn create_junction(link: &Path, target: &Path) -> bool {
        use std::os::windows::process::CommandExt;
        let mut command = std::process::Command::new("cmd");
        command.arg("/C");
        command.raw_arg(format!(
            "mklink /J \"{}\" \"{}\"",
            link.display(),
            target.display()
        ));
        command
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    #[cfg(windows)]
    fn other_volume_temp_dir(tag: &str) -> Option<PathBuf> {
        let current_drive = std::env::temp_dir()
            .to_string_lossy()
            .chars()
            .next()
            .map(|drive| drive.to_ascii_uppercase());
        for drive in 'D'..='Z' {
            if Some(drive) == current_drive {
                continue;
            }
            let root = PathBuf::from(format!("{drive}:\\"));
            if !root.is_dir() {
                continue;
            }
            let dir = root.join(format!(
                "voidlauncher_cross_volume_{}_{}",
                std::process::id(),
                tag
            ));
            let _ = std::fs::remove_dir_all(&dir);
            if std::fs::create_dir_all(&dir).is_err() {
                continue;
            }
            let probe = dir.join("probe");
            if std::fs::write(&probe, b"ok").is_ok() {
                let _ = std::fs::remove_file(&probe);
                return Some(dir);
            }
            let _ = std::fs::remove_dir_all(&dir);
        }
        None
    }

    fn installer(cache: &ProcessorCache, bytes: &[u8]) -> InstallerIdentity {
        write_file(&cache.installer, bytes);
        let checksum = HashSpec::sha1(&sha1_hex(bytes)).unwrap();
        let verification = crate::modloaders::artifact::InstallerVerification {
            checksum,
            source: "test".to_string(),
            url: "https://maven.minecraftforge.net/installer.jar".to_string(),
        };
        let identity = cache.identity_from_verification(&verification).unwrap();
        cache.write_installer_identity(&identity).unwrap();
        identity
    }

    #[test]
    fn installer_identity_survives_and_rejects_changed_bytes() {
        let root = test_root("installer_identity");
        let cache = test_cache(&root);
        cache.prepare().unwrap();
        let identity = installer(&cache, b"installer-bytes");
        let loaded = cache.load_installer_identity().unwrap().unwrap();
        assert!(cache.installer_identity_matches(&loaded).unwrap());
        assert_eq!(loaded.sha256, identity.sha256);
        write_file(&cache.installer, b"tampered");
        assert!(!cache.installer_identity_matches(&loaded).unwrap());
        std::fs::write(&cache.installer_identity, b"{not-json").unwrap();
        assert!(cache.load_installer_identity().unwrap().is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn generation_manifest_binds_inputs_outputs_and_versions() {
        let root = test_root("generation_manifest");
        let cache = test_cache(&root);
        cache.prepare().unwrap();
        let identity = installer(&cache, b"installer-bytes");
        let libs = root.join("libraries");
        let client = root.join("versions/1.20.1/client.jar");
        let output = libs.join("net/minecraft/client/1.20.1-test/client-1.20.1-test-srg.jar");
        let processor = libs.join("net/minecraftforge/installertools/1.0/installertools-1.0.jar");
        write_file(&client, b"client");
        write_file(&output, b"generated-output");
        write_file(&processor, b"processor");
        let processor_digest = file_digest(&processor).unwrap();
        let processor_libraries = vec![ProcessorLibraryIdentity {
            name: "net.minecraftforge:installertools:1.0".to_string(),
            relative_path: "net/minecraftforge/installertools/1.0/installertools-1.0.jar"
                .to_string(),
            sha256: processor_digest.sha256,
            size: processor_digest.size,
        }];
        let processors = vec![ProcessorIdentity {
            jar: "net.minecraftforge:installertools:1.0".to_string(),
            classpath: vec!["net.minecraftforge:installertools:1.0".to_string()],
            args: vec!["--task".to_string()],
        }];
        let expected = vec![ExpectedOutput {
            target: output.clone(),
            relative_path: "net/minecraft/client/1.20.1-test/client-1.20.1-test-srg.jar"
                .to_string(),
            publisher_sha1: None,
            structural_only: true,
        }];
        let output_digest = file_digest(&output).unwrap();
        let manifest = GenerationManifest {
            schema_version: MANIFEST_SCHEMA_VERSION,
            loader: "forge".to_string(),
            mc_version: "1.20.1".to_string(),
            loader_version: "1.20.1-47.4.29".to_string(),
            installer: identity.clone(),
            install_profile_sha256: "profile-sha".to_string(),
            client_jar: file_digest(&client).unwrap(),
            processor_libraries: processor_libraries.clone(),
            processors: processors.clone(),
            outputs: vec![OutputIdentity {
                relative_path: "net/minecraft/client/1.20.1-test/client-1.20.1-test-srg.jar"
                    .to_string(),
                sha256: output_digest.sha256,
                size: output_digest.size,
                publisher_sha1: None,
                structural_only: true,
            }],
        };
        cache.write_generation_manifest(&manifest).unwrap();
        let loaded = cache.load_generation_manifest().unwrap().unwrap();
        cache
            .validate_generation_manifest(
                &loaded,
                "forge",
                "1.20.1",
                "1.20.1-47.4.29",
                &identity,
                "profile-sha",
                &client,
                &libs,
                &processors,
                &processor_libraries,
                &expected,
            )
            .unwrap();
        write_file(&output, b"changed-output");
        assert!(cache
            .validate_generation_manifest(
                &loaded,
                "forge",
                "1.20.1",
                "1.20.1-47.4.29",
                &identity,
                "profile-sha",
                &client,
                &libs,
                &processors,
                &processor_libraries,
                &expected,
            )
            .is_err());
        std::fs::write(&cache.generation_manifest, b"{bad-json").unwrap();
        assert!(cache.load_generation_manifest().unwrap().is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    fn transaction_setup(root: &Path) -> (ProcessorCache, PathBuf, Vec<OutputSpec>) {
        let cache = test_cache(root);
        cache.prepare().unwrap();
        let libs = root.join("libraries");
        write_file(&libs.join("a.jar"), b"old-a");
        write_file(&libs.join("b.jar"), b"old-b");
        cache.begin_staging().unwrap();
        write_file(&cache.staging_dir.join("0-a.jar"), &test_jar("new-a"));
        write_file(&cache.staging_dir.join("1-b.jar"), &test_jar("new-b"));
        let specs = vec![
            OutputSpec {
                target: libs.join("a.jar"),
                staged: cache.staging_dir.join("0-a.jar"),
                publisher_sha1: None,
                structural_only: true,
            },
            OutputSpec {
                target: libs.join("b.jar"),
                staged: cache.staging_dir.join("1-b.jar"),
                publisher_sha1: None,
                structural_only: true,
            },
        ];
        (cache, libs, specs)
    }

    #[test]
    fn transaction_publishes_all_outputs_and_cleans_up() {
        let _guard = TX_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = test_root("transaction_success");
        let (cache, libs, specs) = transaction_setup(&root);
        cache.publish_outputs(&libs, &specs).unwrap();
        assert_eq!(
            std::fs::read(libs.join("a.jar")).unwrap(),
            test_jar("new-a")
        );
        assert_eq!(
            std::fs::read(libs.join("b.jar")).unwrap(),
            test_jar("new-b")
        );
        assert!(!cache.journal.exists());
        assert!(!cache.journal_previous.exists());
        assert!(!cache.staging_dir.exists());
        assert!(!cache.backup_dir.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn processor_worker_owns_cache_lock_after_outer_future_cancellation() {
        use tokio::sync::oneshot;

        let root = test_root("worker_lock_ownership");
        let cache = test_cache(&root);
        cache.prepare().unwrap();
        cache.begin_staging().unwrap();
        let lock = cache.acquire_operation_lock().await;
        let worker_cache = cache.clone();
        let marker = worker_cache.staging_dir.join("worker-marker");
        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let (done_tx, done_rx) = oneshot::channel();
        let outer = tokio::spawn(async move {
            worker_cache
                .run_with_owned_operation_lock(lock, move || {
                    started_tx.send(()).ok();
                    release_rx.blocking_recv().ok();
                    std::fs::write(&marker, b"worker-active").ok();
                    done_tx.send(()).ok();
                })
                .await
                .unwrap();
        });

        started_rx.await.expect("blocking worker started");
        assert!(
            cache.try_acquire_operation_lock().is_none(),
            "worker must hold the operation lock"
        );
        outer.abort();
        assert!(
            cache.try_acquire_operation_lock().is_none(),
            "dropping the awaiting future must not release the worker's lock"
        );

        release_tx.send(()).unwrap();
        done_rx.await.expect("worker completed");
        let mut reacquired = None;
        for _ in 0..100 {
            reacquired = cache.try_acquire_operation_lock();
            if reacquired.is_some() {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(reacquired.is_some(), "worker released the operation lock");
        drop(reacquired);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn transaction_rolls_back_after_injected_publication_error() {
        let _guard = TX_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = test_root("transaction_rollback");
        let (cache, libs, specs) = transaction_setup(&root);
        set_test_error_after_publish(0);
        let result = cache.publish_outputs(&libs, &specs);
        set_test_error_after_publish(-1);
        assert!(result.is_err());
        assert_eq!(std::fs::read(libs.join("a.jar")).unwrap(), b"old-a");
        assert_eq!(std::fs::read(libs.join("b.jar")).unwrap(), b"old-b");
        assert!(!cache.journal.exists());
        assert!(!cache.staging_dir.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn transaction_rollback_removes_a_newly_created_output() {
        let _guard = TX_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = test_root("transaction_new_output");
        let cache = test_cache(&root);
        cache.prepare().unwrap();
        let libs = root.join("libraries");
        write_file(&libs.join("old.jar"), b"old");
        cache.begin_staging().unwrap();
        write_file(&cache.staging_dir.join("0-new.jar"), &test_jar("new"));
        write_file(
            &cache.staging_dir.join("1-old.jar"),
            &test_jar("replacement"),
        );
        let specs = vec![
            OutputSpec {
                target: libs.join("new.jar"),
                staged: cache.staging_dir.join("0-new.jar"),
                publisher_sha1: None,
                structural_only: true,
            },
            OutputSpec {
                target: libs.join("old.jar"),
                staged: cache.staging_dir.join("1-old.jar"),
                publisher_sha1: None,
                structural_only: true,
            },
        ];
        set_test_error_after_publish(0);
        let result = cache.publish_outputs(&libs, &specs);
        set_test_error_after_publish(-1);
        assert!(result.is_err());
        assert!(!libs.join("new.jar").exists());
        assert_eq!(std::fs::read(libs.join("old.jar")).unwrap(), b"old");
        assert!(!cache.journal.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn transaction_rejects_a_checksum_mismatch_before_publication() {
        let _guard = TX_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = test_root("transaction_checksum");
        let (cache, libs, mut specs) = transaction_setup(&root);
        specs[0].publisher_sha1 = Some("0".repeat(40));
        specs[0].structural_only = false;
        let result = cache.publish_outputs(&libs, &specs);
        assert!(result.is_err());
        assert_eq!(std::fs::read(libs.join("a.jar")).unwrap(), b"old-a");
        assert_eq!(std::fs::read(libs.join("b.jar")).unwrap(), b"old-b");
        assert!(!cache.journal.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn corrupted_journal_fails_closed_without_touching_targets() {
        let _guard = TX_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = test_root("transaction_corrupt_journal");
        let cache = test_cache(&root);
        cache.prepare().unwrap();
        let libs = root.join("libraries");
        write_file(&libs.join("a.jar"), b"old-a");
        std::fs::write(&cache.journal, b"{corrupt").unwrap();
        assert!(cache.recover_pending_transaction(&libs).is_err());
        assert_eq!(std::fs::read(libs.join("a.jar")).unwrap(), b"old-a");
        let _ = std::fs::remove_dir_all(&root);
    }

    fn spawn_tx_child(root: &Path, mode: &str, failpoint: &str) -> std::process::ExitStatus {
        let exe = std::env::current_exe().unwrap();
        std::process::Command::new(exe)
            .args([
                "--exact",
                "modloaders::processor_cache::tests::transaction_crash_child",
                "--nocapture",
            ])
            .env("VL_TX_CHILD_ROOT", root)
            .env("VL_TX_CHILD_MODE", mode)
            .env("VOIDLAUNCHER_TX_FAILPOINT", failpoint)
            .status()
            .unwrap()
    }

    #[test]
    fn transaction_crash_child() {
        let Some(root) = std::env::var_os("VL_TX_CHILD_ROOT") else {
            return;
        };
        let root = PathBuf::from(root);
        let mode = std::env::var("VL_TX_CHILD_MODE").unwrap_or_default();
        let cache = test_cache(&root);
        let libs = root.join("libraries");
        if mode == "recover" {
            cache.recover_pending_transaction(&libs).unwrap();
            return;
        }
        let (_, libs, specs) = transaction_setup(&root);
        cache.publish_outputs(&libs, &specs).unwrap();
    }

    #[test]
    fn transaction_recovers_all_crash_stages() {
        let _guard = TX_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for stage in [
            "before_first_publish",
            "after_publish_0",
            "after_publish_1",
            "after_commit",
        ] {
            let root = test_root(&format!("crash_{stage}"));
            let status = spawn_tx_child(&root, "publish", stage);
            assert!(!status.success(), "child should abort at {stage}");
            let cache = test_cache(&root);
            let libs = root.join("libraries");
            cache.recover_pending_transaction(&libs).unwrap();
            let expected_a = if stage == "after_commit" {
                test_jar("new-a")
            } else {
                b"old-a".to_vec()
            };
            let expected_b = if stage == "after_commit" {
                test_jar("new-b")
            } else {
                b"old-b".to_vec()
            };
            assert_eq!(
                std::fs::read(libs.join("a.jar")).unwrap(),
                expected_a,
                "a.jar after {stage}"
            );
            assert_eq!(
                std::fs::read(libs.join("b.jar")).unwrap(),
                expected_b,
                "b.jar after {stage}"
            );
            assert!(!cache.journal.exists(), "journal not cleaned after {stage}");
            assert!(!cache.staging_dir.exists());
            assert!(!cache.backup_dir.exists());
            // Idempotent second recovery.
            cache.recover_pending_transaction(&libs).unwrap();
            assert_eq!(std::fs::read(libs.join("a.jar")).unwrap(), expected_a);
            assert_eq!(std::fs::read(libs.join("b.jar")).unwrap(), expected_b);
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn transaction_recovers_after_repeated_interruption() {
        let _guard = TX_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = test_root("crash_repeated");
        let status = spawn_tx_child(&root, "publish", "after_publish_0");
        assert!(!status.success());
        let status = spawn_tx_child(&root, "recover", "rollback_after_first_restore");
        assert!(!status.success(), "recovery should be interrupted");
        let cache = test_cache(&root);
        let libs = root.join("libraries");
        cache.recover_pending_transaction(&libs).unwrap();
        assert_eq!(std::fs::read(libs.join("a.jar")).unwrap(), b"old-a");
        assert_eq!(std::fs::read(libs.join("b.jar")).unwrap(), b"old-b");
        assert!(!cache.journal.exists());
        assert!(!cache.journal_previous.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_journal_is_a_clean_cache() {
        let root = test_root("journal_missing");
        let cache = test_cache(&root);
        cache.prepare().unwrap();
        let libs = root.join("libraries");
        write_file(&libs.join("a.jar"), b"old-a");
        cache.recover_pending_transaction(&libs).unwrap();
        assert_eq!(std::fs::read(libs.join("a.jar")).unwrap(), b"old-a");
        assert!(!cache.journal.exists());
        assert!(!cache.journal_previous.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn begin_staging_refuses_a_pending_previous_journal_copy() {
        let root = test_root("begin_staging_previous_journal");
        let cache = test_cache(&root);
        cache.prepare().unwrap();
        write_file(&cache.journal_previous, b"{}");
        assert!(cache.begin_staging().is_err());
        assert!(!cache.staging_dir.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn journal_target_parent_traversal_is_rejected() {
        let root = test_root("journal_target_traversal");
        let cache = test_cache(&root);
        cache.prepare().unwrap();
        let libs = root.join("libraries");
        let entry = journal_entry(
            libs.join("sub").join("..").join("..").join("outside.jar"),
            cache.staging_dir.join("0.jar"),
            cache.backup_dir.join("0.bak"),
        );
        let journal = test_journal(vec![entry]);
        assert!(cache.validate_journal_paths(&journal, &libs).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn journal_staging_parent_traversal_is_rejected() {
        let root = test_root("journal_staging_traversal");
        let cache = test_cache(&root);
        cache.prepare().unwrap();
        let libs = root.join("libraries");
        let entry = journal_entry(
            libs.join("a.jar"),
            cache.staging_dir.join("..").join("outside.jar"),
            cache.backup_dir.join("0.bak"),
        );
        let journal = test_journal(vec![entry]);
        assert!(cache.validate_journal_paths(&journal, &libs).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn journal_backup_parent_traversal_is_rejected() {
        let root = test_root("journal_backup_traversal");
        let cache = test_cache(&root);
        cache.prepare().unwrap();
        let libs = root.join("libraries");
        let entry = journal_entry(
            libs.join("a.jar"),
            cache.staging_dir.join("0.jar"),
            cache.backup_dir.join("..").join("outside.bak"),
        );
        let journal = test_journal(vec![entry]);
        assert!(cache.validate_journal_paths(&journal, &libs).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(windows)]
    #[test]
    fn journal_windows_absolute_drive_and_unc_paths_are_rejected() {
        let root = test_root("journal_windows_paths");
        let cache = test_cache(&root);
        cache.prepare().unwrap();
        let libs = root.join("libraries");
        for target in [
            PathBuf::from(r"C:\Windows\Temp\voidlauncher-evil.jar"),
            PathBuf::from(r"\\server\share\voidlauncher-evil.jar"),
            PathBuf::from(r"\\?\C:\Windows\Temp\voidlauncher-evil.jar"),
            PathBuf::from(r"C:voidlauncher-evil.jar"),
        ] {
            let entry = journal_entry(
                target,
                cache.staging_dir.join("0.jar"),
                cache.backup_dir.join("0.bak"),
            );
            let journal = test_journal(vec![entry]);
            assert!(
                cache.validate_journal_paths(&journal, &libs).is_err(),
                "absolute/drive/UNC target must be rejected"
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(windows)]
    #[test]
    fn journal_windows_alternate_data_stream_is_rejected() {
        let root = test_root("journal_ads");
        let cache = test_cache(&root);
        cache.prepare().unwrap();
        let libs = root.join("libraries");
        let entry = journal_entry(
            libs.join("a.jar:evil"),
            cache.staging_dir.join("0.jar"),
            cache.backup_dir.join("0.bak"),
        );
        assert!(cache
            .validate_journal_paths(&test_journal(vec![entry]), &libs)
            .is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(windows)]
    #[test]
    fn journal_file_symlinks_are_rejected() {
        let root = test_root("journal_file_symlink");
        let cache = test_cache(&root);
        cache.prepare().unwrap();
        let libs = root.join("libraries");
        std::fs::create_dir_all(&libs).unwrap();
        let outside = root.join("outside.jar");
        write_file(&outside, &test_jar("outside"));
        let target_link = libs.join("a.jar");
        if std::os::windows::fs::symlink_file(&outside, &target_link).is_err() {
            eprintln!("skipping symlink test: privilege unavailable");
            let _ = std::fs::remove_dir_all(&root);
            return;
        }
        let entry = journal_entry(
            target_link.clone(),
            cache.staging_dir.join("0.jar"),
            cache.backup_dir.join("0.bak"),
        );
        assert!(cache
            .validate_journal_paths(&test_journal(vec![entry]), &libs)
            .is_err());

        std::fs::create_dir_all(&cache.staging_dir).unwrap();
        let staged_link = cache.staging_dir.join("0.jar");
        if std::os::windows::fs::symlink_file(&outside, &staged_link).is_ok() {
            let entry = journal_entry(
                libs.join("b.jar"),
                staged_link,
                cache.backup_dir.join("1.bak"),
            );
            assert!(cache
                .validate_journal_paths(&test_journal(vec![entry]), &libs)
                .is_err());
        }
        let _ = std::fs::remove_file(&target_link);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(windows)]
    #[test]
    fn journal_mixed_separators_cannot_escape_transaction_directories() {
        let root = test_root("journal_mixed_separators");
        let cache = test_cache(&root);
        cache.prepare().unwrap();
        let libs = root.join("libraries");
        let target = PathBuf::from(format!("{}\\sub/..\\..\\outside.jar", libs.display()));
        let entry = journal_entry(
            target,
            cache.staging_dir.join("0.jar"),
            cache.backup_dir.join("0.bak"),
        );
        assert!(cache
            .validate_journal_paths(&test_journal(vec![entry]), &libs)
            .is_err());

        let staged = PathBuf::from(format!("{}\\..\\outside.jar", cache.staging_dir.display()));
        let entry = journal_entry(libs.join("a.jar"), staged, cache.backup_dir.join("0.bak"));
        assert!(cache
            .validate_journal_paths(&test_journal(vec![entry]), &libs)
            .is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(windows)]
    #[test]
    fn journal_target_junction_is_rejected() {
        let root = test_root("journal_target_junction");
        let cache = test_cache(&root);
        cache.prepare().unwrap();
        let libs = root.join("libraries");
        std::fs::create_dir_all(&libs).unwrap();
        let outside = root.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        let link = libs.join("junction");
        if !create_junction(&link, &outside) {
            eprintln!("skipping junction test: mklink /J failed");
            let _ = std::fs::remove_dir_all(&root);
            return;
        }
        let entry = journal_entry(
            link.join("evil.jar"),
            cache.staging_dir.join("0.jar"),
            cache.backup_dir.join("0.bak"),
        );
        assert!(cache
            .validate_journal_paths(&test_journal(vec![entry]), &libs)
            .is_err());
        assert!(!outside.join("evil.jar").exists());
        let _ = std::fs::remove_dir(&link);
        let _ = std::fs::remove_dir_all(&outside);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(windows)]
    #[test]
    fn journal_staging_junction_is_rejected() {
        let root = test_root("journal_staging_junction");
        let cache = test_cache(&root);
        cache.prepare().unwrap();
        let libs = root.join("libraries");
        let outside = root.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        if !create_junction(&cache.staging_dir, &outside) {
            eprintln!("skipping junction test: mklink /J failed");
            let _ = std::fs::remove_dir_all(&root);
            return;
        }
        let entry = journal_entry(
            libs.join("a.jar"),
            cache.staging_dir.join("0.jar"),
            cache.backup_dir.join("0.bak"),
        );
        assert!(cache
            .validate_journal_paths(&test_journal(vec![entry]), &libs)
            .is_err());
        assert!(std::fs::read_dir(&outside).unwrap().next().is_none());
        let _ = std::fs::remove_dir(&cache.staging_dir);
        let _ = std::fs::remove_dir_all(&outside);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(windows)]
    #[test]
    fn journal_backup_junction_is_rejected() {
        let root = test_root("journal_backup_junction");
        let cache = test_cache(&root);
        cache.prepare().unwrap();
        let libs = root.join("libraries");
        let outside = root.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        if !create_junction(&cache.backup_dir, &outside) {
            eprintln!("skipping junction test: mklink /J failed");
            let _ = std::fs::remove_dir_all(&root);
            return;
        }
        let entry = journal_entry(
            libs.join("a.jar"),
            cache.staging_dir.join("0.jar"),
            cache.backup_dir.join("0.bak"),
        );
        assert!(cache
            .validate_journal_paths(&test_journal(vec![entry]), &libs)
            .is_err());
        assert!(std::fs::read_dir(&outside).unwrap().next().is_none());
        let _ = std::fs::remove_dir(&cache.backup_dir);
        let _ = std::fs::remove_dir_all(&outside);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(windows)]
    #[test]
    fn prepare_refuses_to_create_directories_through_a_junction() {
        let root = test_root("prepare_junction");
        let cache = test_cache(&root);
        let outside = root.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        let link = root.join("loader-cache");
        if !create_junction(&link, &outside) {
            eprintln!("skipping junction test: mklink /J failed");
            let _ = std::fs::remove_dir_all(&root);
            return;
        }
        assert!(cache.prepare().is_err());
        assert!(
            std::fs::read_dir(&outside).unwrap().next().is_none(),
            "prepare must not create directories through the junction"
        );
        let _ = std::fs::remove_dir(&link);
        let _ = std::fs::remove_dir_all(&outside);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn oversized_journal_fails_closed() {
        let root = test_root("journal_oversized");
        let cache = test_cache(&root);
        cache.prepare().unwrap();
        let libs = root.join("libraries");
        write_file(&libs.join("a.jar"), b"old-a");
        write_file(&cache.journal, &vec![b'{'; MAX_JSON_BYTES as usize + 1]);
        let error = cache.recover_pending_transaction(&libs).unwrap_err();
        assert!(
            error.to_string().contains("limit") || error.to_string().contains("corrupt"),
            "unexpected error: {error}"
        );
        assert_eq!(std::fs::read(libs.join("a.jar")).unwrap(), b"old-a");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn journal_path_as_directory_fails_closed() {
        let root = test_root("journal_directory");
        let cache = test_cache(&root);
        cache.prepare().unwrap();
        let libs = root.join("libraries");
        std::fs::create_dir_all(&cache.journal).unwrap();
        let error = cache.recover_pending_transaction(&libs).unwrap_err();
        assert!(
            error.to_string().contains("regular file"),
            "unexpected error: {error}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn oversized_journal_blocks_the_generation_manifest_fast_path() {
        let root = test_root("journal_oversized_manifest");
        let cache = test_cache(&root);
        cache.prepare().unwrap();
        let libs = root.join("libraries");
        write_file(&libs.join("a.jar"), b"old-a");
        write_file(&cache.generation_manifest, b"{}");
        write_file(&cache.journal, &vec![b'{'; MAX_JSON_BYTES as usize + 1]);
        assert!(cache.recover_pending_transaction(&libs).is_err());
        assert!(cache.load_generation_manifest().is_err());
        assert_eq!(std::fs::read(libs.join("a.jar")).unwrap(), b"old-a");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn oversized_journal_with_a_valid_previous_copy_recovers() {
        let root = test_root("journal_oversized_redundant");
        let cache = test_cache(&root);
        cache.prepare().unwrap();
        let libs = root.join("libraries");
        write_file(&libs.join("a.jar"), b"old-a");
        write_file(&libs.join("b.jar"), b"old-b");
        let a = libs.join("a.jar");
        let b = libs.join("b.jar");
        let mut first = journal_entry(
            a.clone(),
            cache.staging_dir.join("missing-a.jar"),
            cache.backup_dir.join("0.bak"),
        );
        first.existed = true;
        first.old_sha256 = Some(file_digest(&a).unwrap().sha256);
        first.new_sha256 = "b".repeat(64);
        let mut second = journal_entry(
            b.clone(),
            cache.staging_dir.join("missing-b.jar"),
            cache.backup_dir.join("1.bak"),
        );
        second.existed = true;
        second.old_sha256 = Some(file_digest(&b).unwrap().sha256);
        second.new_sha256 = "c".repeat(64);
        let journal = test_journal(vec![first, second]);
        write_file(
            &cache.journal_previous,
            &serde_json::to_vec_pretty(&journal).unwrap(),
        );
        write_file(&cache.journal, &vec![b'{'; MAX_JSON_BYTES as usize + 1]);
        cache.recover_pending_transaction(&libs).unwrap();
        assert_eq!(std::fs::read(&a).unwrap(), b"old-a");
        assert_eq!(std::fs::read(&b).unwrap(), b"old-b");
        assert!(!cache.journal.exists());
        assert!(!cache.journal_previous.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn partial_publication_with_both_journals_corrupted_fails_closed() {
        let _guard = TX_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = test_root("partial_corrupt_both");
        let status = spawn_tx_child(&root, "publish", "after_publish_0");
        assert!(!status.success());
        let cache = test_cache(&root);
        let libs = root.join("libraries");
        write_file(&cache.journal, b"{corrupt");
        write_file(&cache.journal_previous, b"{corrupt");
        write_file(&cache.generation_manifest, b"{}");

        let error = cache.recover_pending_transaction(&libs).unwrap_err();
        assert!(
            error.to_string().contains("corrupt"),
            "unexpected error: {error}"
        );
        assert_eq!(
            std::fs::read(libs.join("a.jar")).unwrap(),
            test_jar("new-a")
        );
        assert_eq!(std::fs::read(libs.join("b.jar")).unwrap(), b"old-b");
        assert_eq!(
            std::fs::read(cache.backup_dir.join("0.bak")).unwrap(),
            b"old-a"
        );
        assert_eq!(
            std::fs::read(cache.staging_dir.join("1-b.jar")).unwrap(),
            test_jar("new-b")
        );
        assert!(cache.load_generation_manifest().is_err());

        // A repeated recovery must not destroy the only recoverable data.
        assert!(cache.recover_pending_transaction(&libs).is_err());
        assert_eq!(
            std::fs::read(libs.join("a.jar")).unwrap(),
            test_jar("new-a")
        );
        assert_eq!(std::fs::read(libs.join("b.jar")).unwrap(), b"old-b");
        assert!(cache.backup_dir.join("0.bak").is_file());
        assert!(cache.staging_dir.join("1-b.jar").is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn partial_publication_with_one_corrupted_journal_copy_recovers() {
        let _guard = TX_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = test_root("partial_corrupt_one");
        let status = spawn_tx_child(&root, "publish", "after_publish_0");
        assert!(!status.success());
        let cache = test_cache(&root);
        let libs = root.join("libraries");
        write_file(&cache.journal, b"{corrupt");
        cache.recover_pending_transaction(&libs).unwrap();
        assert_eq!(std::fs::read(libs.join("a.jar")).unwrap(), b"old-a");
        assert_eq!(std::fs::read(libs.join("b.jar")).unwrap(), b"old-b");
        assert!(!cache.journal.exists());
        assert!(!cache.journal_previous.exists());
        assert!(!cache.staging_dir.exists());
        assert!(!cache.backup_dir.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn committed_journal_corruption_preserves_new_outputs_and_backups() {
        let _guard = TX_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = test_root("committed_corrupt_both");
        let status = spawn_tx_child(&root, "publish", "after_commit");
        assert!(!status.success());
        let cache = test_cache(&root);
        let libs = root.join("libraries");
        write_file(&cache.journal, b"{corrupt");
        write_file(&cache.journal_previous, b"{corrupt");
        assert!(cache.recover_pending_transaction(&libs).is_err());
        assert_eq!(
            std::fs::read(libs.join("a.jar")).unwrap(),
            test_jar("new-a")
        );
        assert_eq!(
            std::fs::read(libs.join("b.jar")).unwrap(),
            test_jar("new-b")
        );
        assert!(cache.backup_dir.join("0.bak").is_file());
        assert!(cache.backup_dir.join("1.bak").is_file());
        assert!(cache.journal.exists());
        assert!(cache.journal_previous.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn oversized_journal_after_partial_publication_fails_closed() {
        let _guard = TX_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = test_root("partial_oversized");
        let status = spawn_tx_child(&root, "publish", "after_publish_0");
        assert!(!status.success());
        let cache = test_cache(&root);
        let libs = root.join("libraries");
        let oversized = vec![b'{'; MAX_JSON_BYTES as usize + 1];
        write_file(&cache.journal, &oversized);
        write_file(&cache.journal_previous, &oversized);
        assert!(cache.recover_pending_transaction(&libs).is_err());
        assert_eq!(
            std::fs::read(libs.join("a.jar")).unwrap(),
            test_jar("new-a")
        );
        assert_eq!(std::fs::read(libs.join("b.jar")).unwrap(), b"old-b");
        assert!(cache.backup_dir.join("0.bak").is_file());
        assert!(cache.staging_dir.join("1-b.jar").is_file());
        assert!(cache.recover_pending_transaction(&libs).is_err());
        assert!(cache.backup_dir.join("0.bak").is_file());
        assert!(cache.staging_dir.join("1-b.jar").is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn journal_copies_must_agree_at_the_same_generation() {
        let root = test_root("journal_disagree");
        let cache = test_cache(&root);
        cache.prepare().unwrap();
        let libs = root.join("libraries");
        let first = journal_entry(
            libs.join("a.jar"),
            cache.staging_dir.join("0.jar"),
            cache.backup_dir.join("0.bak"),
        );
        let second = journal_entry(
            libs.join("b.jar"),
            cache.staging_dir.join("1.jar"),
            cache.backup_dir.join("1.bak"),
        );
        let primary = test_journal(vec![first]);
        let previous = test_journal(vec![second]);
        write_file(
            &cache.journal,
            &serde_json::to_vec_pretty(&primary).unwrap(),
        );
        write_file(
            &cache.journal_previous,
            &serde_json::to_vec_pretty(&previous).unwrap(),
        );
        let error = cache.recover_pending_transaction(&libs).unwrap_err();
        assert!(
            error.to_string().contains("disagree"),
            "unexpected error: {error}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn generation_manifest_is_not_trusted_while_a_transaction_is_pending() {
        let root = test_root("manifest_pending");
        let cache = test_cache(&root);
        cache.prepare().unwrap();
        write_file(&cache.generation_manifest, b"{}");
        write_file(&cache.journal, b"{corrupt");
        assert!(cache.load_generation_manifest().is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn journal_existing_target_without_old_hash_is_rejected() {
        let root = test_root("schema_existing_no_old");
        let cache = test_cache(&root);
        cache.prepare().unwrap();
        let libs = root.join("libraries");
        let mut entry = journal_entry(
            libs.join("a.jar"),
            cache.staging_dir.join("0.jar"),
            cache.backup_dir.join("0.bak"),
        );
        entry.existed = true;
        entry.old_sha256 = None;
        assert_recovery_rejects(&cache, &libs, &test_journal(vec![entry]));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn journal_new_target_with_an_old_hash_is_rejected() {
        let root = test_root("schema_new_with_old");
        let cache = test_cache(&root);
        cache.prepare().unwrap();
        let libs = root.join("libraries");
        let mut entry = journal_entry(
            libs.join("a.jar"),
            cache.staging_dir.join("0.jar"),
            cache.backup_dir.join("0.bak"),
        );
        entry.existed = false;
        entry.old_sha256 = Some("a".repeat(64));
        assert_recovery_rejects(&cache, &libs, &test_journal(vec![entry]));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn journal_malformed_new_sha256_is_rejected() {
        let root = test_root("schema_bad_new_hash");
        let cache = test_cache(&root);
        cache.prepare().unwrap();
        let libs = root.join("libraries");
        let mut entry = journal_entry(
            libs.join("a.jar"),
            cache.staging_dir.join("0.jar"),
            cache.backup_dir.join("0.bak"),
        );
        entry.new_sha256 = "not-a-sha256".to_string();
        assert_recovery_rejects(&cache, &libs, &test_journal(vec![entry]));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn journal_malformed_old_sha256_is_rejected() {
        let root = test_root("schema_bad_old_hash");
        let cache = test_cache(&root);
        cache.prepare().unwrap();
        let libs = root.join("libraries");
        let mut entry = journal_entry(
            libs.join("a.jar"),
            cache.staging_dir.join("0.jar"),
            cache.backup_dir.join("0.bak"),
        );
        entry.existed = true;
        entry.old_sha256 = Some("not-a-sha256".to_string());
        assert_recovery_rejects(&cache, &libs, &test_journal(vec![entry]));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn journal_malformed_publisher_sha1_is_rejected() {
        let root = test_root("schema_bad_publisher_hash");
        let cache = test_cache(&root);
        cache.prepare().unwrap();
        let libs = root.join("libraries");
        let mut entry = journal_entry(
            libs.join("a.jar"),
            cache.staging_dir.join("0.jar"),
            cache.backup_dir.join("0.bak"),
        );
        entry.structural_only = false;
        entry.publisher_sha1 = Some("not-a-sha1".to_string());
        assert_recovery_rejects(&cache, &libs, &test_journal(vec![entry]));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn journal_duplicate_target_paths_are_rejected() {
        let root = test_root("schema_duplicate_target");
        let cache = test_cache(&root);
        cache.prepare().unwrap();
        let libs = root.join("libraries");
        let target = libs.join("a.jar");
        let first = journal_entry(
            target.clone(),
            cache.staging_dir.join("0.jar"),
            cache.backup_dir.join("0.bak"),
        );
        let second = journal_entry(
            target,
            cache.staging_dir.join("1.jar"),
            cache.backup_dir.join("1.bak"),
        );
        assert_recovery_rejects(&cache, &libs, &test_journal(vec![first, second]));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn journal_duplicate_staging_paths_are_rejected() {
        let root = test_root("schema_duplicate_staging");
        let cache = test_cache(&root);
        cache.prepare().unwrap();
        let libs = root.join("libraries");
        let staged = cache.staging_dir.join("0.jar");
        let first = journal_entry(
            libs.join("a.jar"),
            staged.clone(),
            cache.backup_dir.join("0.bak"),
        );
        let second = journal_entry(libs.join("b.jar"), staged, cache.backup_dir.join("1.bak"));
        assert_recovery_rejects(&cache, &libs, &test_journal(vec![first, second]));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn journal_duplicate_backup_paths_are_rejected() {
        let root = test_root("schema_duplicate_backup");
        let cache = test_cache(&root);
        cache.prepare().unwrap();
        let libs = root.join("libraries");
        let backup = cache.backup_dir.join("0.bak");
        let first = journal_entry(
            libs.join("a.jar"),
            cache.staging_dir.join("0.jar"),
            backup.clone(),
        );
        let second = journal_entry(libs.join("b.jar"), cache.staging_dir.join("1.jar"), backup);
        assert_recovery_rejects(&cache, &libs, &test_journal(vec![first, second]));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn journal_overlapping_target_staging_and_backup_paths_are_rejected() {
        let root = test_root("schema_overlapping");
        let cache = test_cache(&root);
        cache.prepare().unwrap();
        let libs = root.join("libraries");
        let target = libs.join("a.jar");
        let staged = cache.staging_dir.join("0.jar");
        let backup = cache.backup_dir.join("0.bak");
        for (target, staged, backup) in [
            (target.clone(), target.clone(), backup.clone()),
            (target.clone(), staged.clone(), target.clone()),
            (target, staged.clone(), staged),
        ] {
            let entry = journal_entry(target, staged, backup);
            assert_recovery_rejects(&cache, &libs, &test_journal(vec![entry]));
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn journal_inconsistent_provenance_flags_are_rejected() {
        let root = test_root("schema_provenance_flags");
        let cache = test_cache(&root);
        cache.prepare().unwrap();
        let libs = root.join("libraries");

        let mut publisher_without_checksum_mode = journal_entry(
            libs.join("a.jar"),
            cache.staging_dir.join("0.jar"),
            cache.backup_dir.join("0.bak"),
        );
        publisher_without_checksum_mode.publisher_sha1 = Some("a".repeat(40));
        publisher_without_checksum_mode.structural_only = true;
        assert_recovery_rejects(
            &cache,
            &libs,
            &test_journal(vec![publisher_without_checksum_mode]),
        );

        let mut checksum_mode_without_publisher = journal_entry(
            libs.join("b.jar"),
            cache.staging_dir.join("1.jar"),
            cache.backup_dir.join("1.bak"),
        );
        checksum_mode_without_publisher.structural_only = false;
        checksum_mode_without_publisher.publisher_sha1 = None;
        assert_recovery_rejects(
            &cache,
            &libs,
            &test_journal(vec![checksum_mode_without_publisher]),
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn journal_with_no_entries_is_rejected() {
        let root = test_root("schema_empty");
        let cache = test_cache(&root);
        cache.prepare().unwrap();
        let libs = root.join("libraries");
        assert_recovery_rejects(&cache, &libs, &test_journal(Vec::new()));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn journal_with_zero_generation_is_rejected() {
        let root = test_root("schema_zero_generation");
        let cache = test_cache(&root);
        cache.prepare().unwrap();
        let libs = root.join("libraries");
        let entry = journal_entry(
            libs.join("a.jar"),
            cache.staging_dir.join("0.jar"),
            cache.backup_dir.join("0.bak"),
        );
        let mut journal = test_journal(vec![entry]);
        journal.generation = 0;
        journal.seal().unwrap();
        assert_recovery_rejects(&cache, &libs, &journal);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn journal_with_an_invalid_transaction_id_is_rejected() {
        let root = test_root("schema_bad_tx_id");
        let cache = test_cache(&root);
        cache.prepare().unwrap();
        let libs = root.join("libraries");
        let entry = journal_entry(
            libs.join("a.jar"),
            cache.staging_dir.join("0.jar"),
            cache.backup_dir.join("0.bak"),
        );
        let mut journal = test_journal(vec![entry]);
        journal.tx_id = "not-a-uuid".to_string();
        journal.seal().unwrap();
        assert_recovery_rejects(&cache, &libs, &journal);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn publish_rejects_duplicate_targets_before_touching_old_outputs() {
        let _guard = TX_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = test_root("publish_duplicate_target");
        let (cache, libs, mut specs) = transaction_setup(&root);
        specs[1].target = specs[0].target.clone();
        assert!(cache.publish_outputs(&libs, &specs).is_err());
        assert_eq!(std::fs::read(libs.join("a.jar")).unwrap(), b"old-a");
        assert_eq!(std::fs::read(libs.join("b.jar")).unwrap(), b"old-b");
        assert!(!cache.journal.exists());
        assert!(!cache.journal_previous.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn publish_rejects_duplicate_staging_paths_before_touching_old_outputs() {
        let _guard = TX_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = test_root("publish_duplicate_staging");
        let (cache, libs, mut specs) = transaction_setup(&root);
        specs[1].staged = specs[0].staged.clone();
        assert!(cache.publish_outputs(&libs, &specs).is_err());
        assert_eq!(std::fs::read(libs.join("a.jar")).unwrap(), b"old-a");
        assert_eq!(std::fs::read(libs.join("b.jar")).unwrap(), b"old-b");
        assert!(!cache.journal.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(windows)]
    #[test]
    fn publish_rejects_case_insensitive_duplicate_targets() {
        let _guard = TX_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = test_root("publish_case_duplicate");
        let cache = test_cache(&root);
        cache.prepare().unwrap();
        let libs = root.join("libraries");
        write_file(&libs.join("a.jar"), b"old-a");
        cache.begin_staging().unwrap();
        write_file(&cache.staging_dir.join("0.jar"), &test_jar("0"));
        write_file(&cache.staging_dir.join("1.jar"), &test_jar("1"));
        let specs = vec![
            OutputSpec {
                target: libs.join("a.jar"),
                staged: cache.staging_dir.join("0.jar"),
                publisher_sha1: None,
                structural_only: true,
            },
            OutputSpec {
                target: libs.join("A.jar"),
                staged: cache.staging_dir.join("1.jar"),
                publisher_sha1: None,
                structural_only: true,
            },
        ];
        assert!(cache.publish_outputs(&libs, &specs).is_err());
        assert_eq!(std::fs::read(libs.join("a.jar")).unwrap(), b"old-a");
        assert!(!cache.journal.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(windows)]
    #[test]
    fn publish_rejects_mixed_separator_duplicate_targets() {
        let _guard = TX_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = test_root("publish_separator_duplicate");
        let cache = test_cache(&root);
        cache.prepare().unwrap();
        let libs = root.join("libraries");
        write_file(&libs.join("a.jar"), b"old-a");
        cache.begin_staging().unwrap();
        write_file(&cache.staging_dir.join("0.jar"), &test_jar("0"));
        write_file(&cache.staging_dir.join("1.jar"), &test_jar("1"));
        let specs = vec![
            OutputSpec {
                target: libs.join("a.jar"),
                staged: cache.staging_dir.join("0.jar"),
                publisher_sha1: None,
                structural_only: true,
            },
            OutputSpec {
                target: PathBuf::from(format!("{}\\a.jar", libs.display())),
                staged: cache.staging_dir.join("1.jar"),
                publisher_sha1: None,
                structural_only: true,
            },
        ];
        assert!(cache.publish_outputs(&libs, &specs).is_err());
        assert_eq!(std::fs::read(libs.join("a.jar")).unwrap(), b"old-a");
        assert!(!cache.journal.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn publish_rejects_dot_component_duplicate_targets() {
        let _guard = TX_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = test_root("publish_dot_duplicate");
        let cache = test_cache(&root);
        cache.prepare().unwrap();
        let libs = root.join("libraries");
        write_file(&libs.join("a.jar"), b"old-a");
        cache.begin_staging().unwrap();
        write_file(&cache.staging_dir.join("0.jar"), &test_jar("0"));
        write_file(&cache.staging_dir.join("1.jar"), &test_jar("1"));
        let specs = vec![
            OutputSpec {
                target: libs.join("a.jar"),
                staged: cache.staging_dir.join("0.jar"),
                publisher_sha1: None,
                structural_only: true,
            },
            OutputSpec {
                target: libs.join(".").join("a.jar"),
                staged: cache.staging_dir.join("1.jar"),
                publisher_sha1: None,
                structural_only: true,
            },
        ];
        assert!(cache.publish_outputs(&libs, &specs).is_err());
        assert_eq!(std::fs::read(libs.join("a.jar")).unwrap(), b"old-a");
        assert!(!cache.journal.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn publish_rejects_inconsistent_provenance_flags() {
        let _guard = TX_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = test_root("publish_bad_flags");
        let (cache, libs, mut specs) = transaction_setup(&root);
        specs[0].publisher_sha1 = Some("a".repeat(40));
        specs[0].structural_only = true;
        assert!(cache.publish_outputs(&libs, &specs).is_err());
        assert_eq!(std::fs::read(libs.join("a.jar")).unwrap(), b"old-a");
        assert_eq!(std::fs::read(libs.join("b.jar")).unwrap(), b"old-b");
        assert!(!cache.journal.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn publish_rejects_missing_staged_output_before_touching_targets() {
        let _guard = TX_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = test_root("publish_missing_staged");
        let (cache, libs, specs) = transaction_setup(&root);
        std::fs::remove_file(&specs[0].staged).unwrap();
        assert!(cache.publish_outputs(&libs, &specs).is_err());
        assert_eq!(std::fs::read(libs.join("a.jar")).unwrap(), b"old-a");
        assert_eq!(std::fs::read(libs.join("b.jar")).unwrap(), b"old-b");
        assert!(!cache.journal.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn publish_rejects_structural_only_staging_that_is_not_a_jar() {
        let _guard = TX_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = test_root("publish_bad_staged_jar");
        let (cache, libs, specs) = transaction_setup(&root);
        write_file(&specs[0].staged, b"not-a-jar");
        assert!(cache.publish_outputs(&libs, &specs).is_err());
        assert_eq!(std::fs::read(libs.join("a.jar")).unwrap(), b"old-a");
        assert_eq!(std::fs::read(libs.join("b.jar")).unwrap(), b"old-b");
        assert!(!cache.journal.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn publish_rejects_staged_output_mutated_after_verification() {
        let _guard = TX_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = test_root("publish_mutated_staged");
        let (cache, libs, specs) = transaction_setup(&root);
        set_test_mutate_staged_output(0);
        let result = cache.publish_outputs(&libs, &specs);
        set_test_mutate_staged_output(-1);
        assert!(result.is_err());
        assert_eq!(std::fs::read(libs.join("a.jar")).unwrap(), b"old-a");
        assert_eq!(std::fs::read(libs.join("b.jar")).unwrap(), b"old-b");
        assert!(!cache.journal.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn publish_rejects_staged_output_replaced_by_a_directory() {
        let _guard = TX_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = test_root("publish_staged_directory");
        let (cache, libs, specs) = transaction_setup(&root);
        std::fs::remove_file(&specs[0].staged).unwrap();
        std::fs::create_dir_all(&specs[0].staged).unwrap();
        assert!(cache.publish_outputs(&libs, &specs).is_err());
        assert_eq!(std::fs::read(libs.join("a.jar")).unwrap(), b"old-a");
        assert_eq!(std::fs::read(libs.join("b.jar")).unwrap(), b"old-b");
        assert!(!cache.journal.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(windows)]
    #[test]
    fn publish_rejects_a_staging_directory_replaced_by_a_junction() {
        let _guard = TX_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = test_root("publish_staging_junction");
        let (cache, libs, specs) = transaction_setup(&root);
        let outside = root.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::remove_dir_all(&cache.staging_dir).unwrap();
        if !create_junction(&cache.staging_dir, &outside) {
            eprintln!("skipping junction test: mklink /J failed");
            let _ = std::fs::remove_dir_all(&root);
            return;
        }
        assert!(cache.publish_outputs(&libs, &specs).is_err());
        assert_eq!(std::fs::read(libs.join("a.jar")).unwrap(), b"old-a");
        assert_eq!(std::fs::read(libs.join("b.jar")).unwrap(), b"old-b");
        assert!(std::fs::read_dir(&outside).unwrap().next().is_none());
        let _ = std::fs::remove_dir(&cache.staging_dir);
        let _ = std::fs::remove_dir_all(&outside);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn publish_rename_target_to_backup_failure_leaves_targets_untouched() {
        let _guard = TX_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = test_root("publish_fail_preserve");
        let (cache, libs, specs) = transaction_setup(&root);
        set_test_rename_failure(Some("target_to_backup"), 0);
        let result = cache.publish_outputs(&libs, &specs);
        set_test_rename_failure(None, -1);
        assert!(result.is_err());
        assert_eq!(std::fs::read(libs.join("a.jar")).unwrap(), b"old-a");
        assert_eq!(std::fs::read(libs.join("b.jar")).unwrap(), b"old-b");
        assert!(!cache.journal.exists());
        assert!(!cache.staging_dir.exists());
        assert!(!cache.backup_dir.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn publish_rename_staged_to_target_failure_restores_backup() {
        let _guard = TX_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = test_root("publish_fail_staged_rename");
        let (cache, libs, specs) = transaction_setup(&root);
        set_test_rename_failure(Some("staged_to_target"), 0);
        let result = cache.publish_outputs(&libs, &specs);
        set_test_rename_failure(None, -1);
        assert!(result.is_err());
        assert_eq!(std::fs::read(libs.join("a.jar")).unwrap(), b"old-a");
        assert_eq!(std::fs::read(libs.join("b.jar")).unwrap(), b"old-b");
        assert!(!cache.journal.exists());
        assert!(!cache.staging_dir.exists());
        assert!(!cache.backup_dir.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rollback_backup_rename_failure_preserves_backup_for_retry() {
        let _guard = TX_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = test_root("rollback_fail_restore");
        let (cache, libs, specs) = transaction_setup(&root);
        set_test_error_after_publish(0);
        set_test_rename_failure(Some("backup_to_target"), 0);
        let result = cache.publish_outputs(&libs, &specs);
        set_test_error_after_publish(-1);
        set_test_rename_failure(None, -1);
        assert!(result.is_err());
        assert_eq!(
            std::fs::read(libs.join("a.jar")).unwrap(),
            test_jar("new-a")
        );
        assert_eq!(std::fs::read(libs.join("b.jar")).unwrap(), b"old-b");
        assert!(cache.backup_dir.join("0.bak").is_file());
        assert!(cache.journal.exists());

        cache.recover_pending_transaction(&libs).unwrap();
        assert_eq!(std::fs::read(libs.join("a.jar")).unwrap(), b"old-a");
        assert_eq!(std::fs::read(libs.join("b.jar")).unwrap(), b"old-b");
        assert!(!cache.journal.exists());
        assert!(!cache.backup_dir.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rollback_restore_hash_mismatch_preserves_the_backup() {
        let _guard = TX_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = test_root("rollback_hash_mismatch");
        let (cache, libs, specs) = transaction_setup(&root);
        set_test_error_after_publish(0);
        set_test_corrupt_restored_target(true);
        let result = cache.publish_outputs(&libs, &specs);
        set_test_error_after_publish(-1);
        set_test_corrupt_restored_target(false);
        assert!(result.is_err());
        assert_eq!(
            std::fs::read(cache.backup_dir.join("0.bak")).unwrap(),
            b"old-a"
        );
        assert!(cache.journal.exists());

        cache.recover_pending_transaction(&libs).unwrap();
        assert_eq!(std::fs::read(libs.join("a.jar")).unwrap(), b"old-a");
        assert_eq!(std::fs::read(libs.join("b.jar")).unwrap(), b"old-b");
        assert!(!cache.journal.exists());
        assert!(!cache.backup_dir.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn publish_cleanup_failure_keeps_the_journal_for_retry() {
        let _guard = TX_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = test_root("publish_cleanup_failure");
        let (cache, libs, specs) = transaction_setup(&root);
        set_test_cleanup_failure(Some("staging"));
        let result = cache.publish_outputs(&libs, &specs);
        set_test_cleanup_failure(None);
        assert!(result.is_err());
        assert_eq!(
            std::fs::read(libs.join("a.jar")).unwrap(),
            test_jar("new-a")
        );
        assert_eq!(
            std::fs::read(libs.join("b.jar")).unwrap(),
            test_jar("new-b")
        );
        assert!(cache.journal.exists());
        assert!(cache.journal_previous.exists());

        cache.recover_pending_transaction(&libs).unwrap();
        assert_eq!(
            std::fs::read(libs.join("a.jar")).unwrap(),
            test_jar("new-a")
        );
        assert_eq!(
            std::fs::read(libs.join("b.jar")).unwrap(),
            test_jar("new-b")
        );
        assert!(!cache.journal.exists());
        assert!(!cache.journal_previous.exists());
        assert!(!cache.staging_dir.exists());
        assert!(!cache.backup_dir.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn publish_cleanup_backup_failure_keeps_the_journal_for_retry() {
        let _guard = TX_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = test_root("publish_cleanup_backup_failure");
        let (cache, libs, specs) = transaction_setup(&root);
        set_test_cleanup_failure(Some("backup"));
        let result = cache.publish_outputs(&libs, &specs);
        set_test_cleanup_failure(None);
        assert!(result.is_err());
        assert_eq!(
            std::fs::read(libs.join("a.jar")).unwrap(),
            test_jar("new-a")
        );
        assert_eq!(
            std::fs::read(libs.join("b.jar")).unwrap(),
            test_jar("new-b")
        );
        assert!(cache.journal.exists());
        assert!(cache.backup_dir.exists());

        cache.recover_pending_transaction(&libs).unwrap();
        assert_eq!(
            std::fs::read(libs.join("a.jar")).unwrap(),
            test_jar("new-a")
        );
        assert_eq!(
            std::fs::read(libs.join("b.jar")).unwrap(),
            test_jar("new-b")
        );
        assert!(!cache.journal.exists());
        assert!(!cache.backup_dir.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn corrupted_journal_after_the_second_publication_preserves_backups() {
        let _guard = TX_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = test_root("partial_corrupt_second");
        let status = spawn_tx_child(&root, "publish", "after_publish_1");
        assert!(!status.success());
        let cache = test_cache(&root);
        let libs = root.join("libraries");
        write_file(&cache.journal, b"{corrupt");
        write_file(&cache.journal_previous, b"{corrupt");
        assert!(cache.recover_pending_transaction(&libs).is_err());
        assert_eq!(
            std::fs::read(libs.join("a.jar")).unwrap(),
            test_jar("new-a")
        );
        assert_eq!(
            std::fs::read(libs.join("b.jar")).unwrap(),
            test_jar("new-b")
        );
        assert_eq!(
            std::fs::read(cache.backup_dir.join("0.bak")).unwrap(),
            b"old-a"
        );
        assert_eq!(
            std::fs::read(cache.backup_dir.join("1.bak")).unwrap(),
            b"old-b"
        );
        assert!(cache.journal.exists());
        assert!(cache.journal_previous.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn loader_cache_operation_lock_serializes_the_same_root() {
        let root = test_root("operation_lock");
        let cache = test_cache(&root);
        let guard = cache.acquire_operation_lock().await;
        assert!(cache.operation_lock_is_held());
        let second = test_cache(&root);
        assert!(second.operation_lock_is_held());
        drop(guard);
        assert!(!cache.operation_lock_is_held());
        let _second_guard = second.acquire_operation_lock().await;
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(windows)]
    #[test]
    fn journal_windows_trailing_dot_and_space_aliases_are_rejected() {
        let root = test_root("journal_trailing_alias");
        let cache = test_cache(&root);
        cache.prepare().unwrap();
        let libs = root.join("libraries");
        for name in ["a.jar.", "a.jar ", "a..."] {
            let entry = journal_entry(
                libs.join(name),
                cache.staging_dir.join("0.jar"),
                cache.backup_dir.join("0.bak"),
            );
            assert!(
                cache
                    .validate_journal_paths(&test_journal(vec![entry]), &libs)
                    .is_err(),
                "trailing dot/space alias must be rejected: {name:?}"
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn publish_rejects_a_directory_at_the_target_path() {
        let _guard = TX_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = test_root("publish_target_directory");
        let cache = test_cache(&root);
        cache.prepare().unwrap();
        let libs = root.join("libraries");
        std::fs::create_dir_all(libs.join("a.jar")).unwrap();
        cache.begin_staging().unwrap();
        write_file(&cache.staging_dir.join("0.jar"), &test_jar("new"));
        let specs = vec![OutputSpec {
            target: libs.join("a.jar"),
            staged: cache.staging_dir.join("0.jar"),
            publisher_sha1: None,
            structural_only: true,
        }];
        assert!(cache.publish_outputs(&libs, &specs).is_err());
        assert!(libs.join("a.jar").is_dir());
        assert!(!cache.journal.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(windows)]
    #[test]
    fn cross_volume_libraries_junction_publishes_and_rolls_back() {
        let _guard = TX_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = test_root("cross_volume");
        let cache = test_cache(&root);
        let Some(other) = other_volume_temp_dir("libraries") else {
            eprintln!("skipping cross-volume test: no second writable volume");
            let _ = std::fs::remove_dir_all(&root);
            return;
        };
        let libs = root.join("libraries");
        if !create_junction(&libs, &other) {
            eprintln!("skipping cross-volume test: mklink /J to another volume failed");
            let _ = std::fs::remove_dir_all(&other);
            let _ = std::fs::remove_dir_all(&root);
            return;
        }

        write_file(&libs.join("a.jar"), b"old-a");
        write_file(&libs.join("b.jar"), b"old-b");
        cache.begin_staging().unwrap();
        write_file(&cache.staging_dir.join("0-a.jar"), &test_jar("new-a"));
        write_file(&cache.staging_dir.join("1-b.jar"), &test_jar("new-b"));
        let specs = vec![
            OutputSpec {
                target: libs.join("a.jar"),
                staged: cache.staging_dir.join("0-a.jar"),
                publisher_sha1: None,
                structural_only: true,
            },
            OutputSpec {
                target: libs.join("b.jar"),
                staged: cache.staging_dir.join("1-b.jar"),
                publisher_sha1: None,
                structural_only: true,
            },
        ];
        cache.publish_outputs(&libs, &specs).unwrap();
        assert_eq!(
            std::fs::read(libs.join("a.jar")).unwrap(),
            test_jar("new-a")
        );
        assert_eq!(
            std::fs::read(libs.join("b.jar")).unwrap(),
            test_jar("new-b")
        );
        assert!(!cache.journal.exists());
        assert!(!cache.journal_previous.exists());
        assert!(!cache.staging_dir.exists());
        assert!(!cache.backup_dir.exists());

        // Exercise rollback across the junction: backups live on the cache
        // volume, targets live on the other volume.
        write_file(&libs.join("a.jar"), b"old-a");
        write_file(&libs.join("b.jar"), b"old-b");
        cache.begin_staging().unwrap();
        write_file(&cache.staging_dir.join("0-a.jar"), &test_jar("new-a"));
        write_file(&cache.staging_dir.join("1-b.jar"), &test_jar("new-b"));
        set_test_error_after_publish(0);
        let result = cache.publish_outputs(&libs, &specs);
        set_test_error_after_publish(-1);
        assert!(result.is_err());
        assert_eq!(std::fs::read(libs.join("a.jar")).unwrap(), b"old-a");
        assert_eq!(std::fs::read(libs.join("b.jar")).unwrap(), b"old-b");
        assert!(!cache.journal.exists());
        assert!(!cache.staging_dir.exists());
        assert!(!cache.backup_dir.exists());

        let _ = std::fs::remove_dir(&libs);
        let _ = std::fs::remove_dir_all(&other);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(windows)]
    #[test]
    fn publish_cross_device_target_to_backup_uses_the_copy_fallback() {
        let _guard = TX_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = test_root("cross_device_preserve");
        let (cache, libs, specs) = transaction_setup(&root);
        set_test_cross_device_rename(Some("target_to_backup"), 0);
        let result = cache.publish_outputs(&libs, &specs);
        set_test_cross_device_rename(None, -1);
        assert!(result.is_ok(), "cross-device preserve failed: {result:?}");
        assert_eq!(
            std::fs::read(libs.join("a.jar")).unwrap(),
            test_jar("new-a")
        );
        assert_eq!(
            std::fs::read(libs.join("b.jar")).unwrap(),
            test_jar("new-b")
        );
        assert!(!cache.journal.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(windows)]
    #[test]
    fn publish_cross_device_staged_to_target_uses_the_copy_fallback() {
        let _guard = TX_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = test_root("cross_device_publish");
        let (cache, libs, specs) = transaction_setup(&root);
        set_test_cross_device_rename(Some("staged_to_target"), 0);
        let result = cache.publish_outputs(&libs, &specs);
        set_test_cross_device_rename(None, -1);
        assert!(result.is_ok(), "cross-device publish failed: {result:?}");
        assert_eq!(
            std::fs::read(libs.join("a.jar")).unwrap(),
            test_jar("new-a")
        );
        assert_eq!(
            std::fs::read(libs.join("b.jar")).unwrap(),
            test_jar("new-b")
        );
        assert!(!cache.journal.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn committed_recovery_with_a_damaged_target_requires_repair_without_rollback() {
        let _guard = TX_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = test_root("committed_damaged_target");
        let status = spawn_tx_child(&root, "publish", "after_commit");
        assert!(!status.success());
        let cache = test_cache(&root);
        let libs = root.join("libraries");
        std::fs::remove_file(libs.join("a.jar")).unwrap();
        write_file(&cache.generation_manifest, b"{}");
        let before_b = std::fs::read(libs.join("b.jar")).unwrap();

        let error = cache.recover_pending_transaction(&libs).unwrap_err();
        assert!(
            error.to_string().contains("Committed processor generation"),
            "unexpected error: {error}"
        );
        assert!(
            !libs.join("a.jar").exists(),
            "committed recovery must not restore old a.jar"
        );
        assert_eq!(std::fs::read(libs.join("b.jar")).unwrap(), before_b);
        assert!(cache.backup_dir.join("0.bak").is_file());
        assert!(cache.backup_dir.join("1.bak").is_file());
        assert!(cache.journal.exists());
        assert!(cache.journal_previous.exists());
        assert!(cache.load_generation_manifest().is_err());

        assert!(cache.recover_pending_transaction(&libs).is_err());
        assert!(!libs.join("a.jar").exists());
        assert_eq!(std::fs::read(libs.join("b.jar")).unwrap(), before_b);
        assert!(cache.backup_dir.join("0.bak").is_file());
        assert!(cache.backup_dir.join("1.bak").is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn committed_recovery_with_a_missing_old_backup_preserves_the_rest() {
        let _guard = TX_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = test_root("committed_missing_backup");
        let status = spawn_tx_child(&root, "publish", "after_commit");
        assert!(!status.success());
        let cache = test_cache(&root);
        let libs = root.join("libraries");
        // Simulate a cleanup that deleted the first backup before crashing.
        std::fs::remove_file(cache.backup_dir.join("0.bak")).unwrap();
        std::fs::remove_file(libs.join("a.jar")).unwrap();
        let before_b = std::fs::read(libs.join("b.jar")).unwrap();

        assert!(cache.recover_pending_transaction(&libs).is_err());
        assert!(!libs.join("a.jar").exists());
        assert_eq!(std::fs::read(libs.join("b.jar")).unwrap(), before_b);
        assert!(!cache.backup_dir.join("0.bak").exists());
        assert!(cache.backup_dir.join("1.bak").is_file());
        assert!(cache.journal.exists());

        assert!(cache.recover_pending_transaction(&libs).is_err());
        assert!(!libs.join("a.jar").exists());
        assert_eq!(std::fs::read(libs.join("b.jar")).unwrap(), before_b);
        assert!(cache.backup_dir.join("1.bak").is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn committed_recovery_with_all_backups_still_does_not_roll_back() {
        let _guard = TX_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = test_root("committed_no_rollback");
        let status = spawn_tx_child(&root, "publish", "after_commit");
        assert!(!status.success());
        let cache = test_cache(&root);
        let libs = root.join("libraries");
        write_file(&libs.join("a.jar"), b"damaged-after-commit");
        let before_b = std::fs::read(libs.join("b.jar")).unwrap();

        assert!(cache.recover_pending_transaction(&libs).is_err());
        assert_eq!(
            std::fs::read(libs.join("a.jar")).unwrap(),
            b"damaged-after-commit"
        );
        assert_eq!(std::fs::read(libs.join("b.jar")).unwrap(), before_b);
        assert!(cache.backup_dir.join("0.bak").is_file());
        assert!(cache.backup_dir.join("1.bak").is_file());
        assert!(cache.journal.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn crash_between_prepared_journal_copies_uses_the_secondary() {
        let _guard = TX_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = test_root("journal_crash_prepared");
        let status = spawn_tx_child(&root, "publish", "after_secondary_prepared_journal");
        assert!(!status.success());
        let cache = test_cache(&root);
        let libs = root.join("libraries");
        let journal = cache.load_journal().unwrap().unwrap();
        assert_eq!(journal.stage, JournalStage::Prepared);
        assert_eq!(journal.generation, 1);
        assert_eq!(std::fs::read(libs.join("a.jar")).unwrap(), b"old-a");
        cache.recover_pending_transaction(&libs).unwrap();
        assert_eq!(std::fs::read(libs.join("a.jar")).unwrap(), b"old-a");
        assert_eq!(std::fs::read(libs.join("b.jar")).unwrap(), b"old-b");
        assert!(!cache.journal.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn crash_between_committed_journal_copies_uses_the_secondary() {
        let _guard = TX_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = test_root("journal_crash_committed");
        let status = spawn_tx_child(&root, "publish", "after_secondary_committed_journal");
        assert!(!status.success());
        let cache = test_cache(&root);
        let libs = root.join("libraries");
        let journal = cache.load_journal().unwrap().unwrap();
        assert_eq!(journal.stage, JournalStage::Committed);
        assert_eq!(journal.generation, 2);
        assert!(
            !cache.journal.exists(),
            "the stale primary must have been invalidated before the new secondary"
        );
        assert_eq!(
            std::fs::read(libs.join("a.jar")).unwrap(),
            test_jar("new-a")
        );
        assert_eq!(
            std::fs::read(libs.join("b.jar")).unwrap(),
            test_jar("new-b")
        );
        cache.recover_pending_transaction(&libs).unwrap();
        assert_eq!(
            std::fs::read(libs.join("a.jar")).unwrap(),
            test_jar("new-a")
        );
        assert_eq!(
            std::fs::read(libs.join("b.jar")).unwrap(),
            test_jar("new-b")
        );
        assert!(!cache.journal.exists());
        assert!(!cache.backup_dir.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn journal_secondary_failures_abort_before_publication() {
        let _guard = TX_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for stage in ["write", "sync", "rename"] {
            let root = test_root(&format!("journal_secondary_{stage}"));
            let (cache, libs, specs) = transaction_setup(&root);
            set_test_journal_write_failure(Some((1, "secondary", stage)));
            let result = cache.publish_outputs(&libs, &specs);
            set_test_journal_write_failure(None);
            assert!(result.is_err(), "secondary {stage} failure must abort");
            assert_eq!(std::fs::read(libs.join("a.jar")).unwrap(), b"old-a");
            assert_eq!(std::fs::read(libs.join("b.jar")).unwrap(), b"old-b");
            assert!(!cache.journal.exists());
            assert!(!cache.journal_previous.exists());
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn journal_commit_secondary_failure_rolls_back_the_old_generation() {
        let _guard = TX_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for stage in ["write", "sync", "rename"] {
            let root = test_root(&format!("journal_commit_secondary_failure_{stage}"));
            let (cache, libs, specs) = transaction_setup(&root);
            set_test_journal_write_failure(Some((2, "secondary", stage)));
            let result = cache.publish_outputs(&libs, &specs);
            set_test_journal_write_failure(None);
            assert!(result.is_err(), "commit secondary {stage} must fail");
            assert_eq!(std::fs::read(libs.join("a.jar")).unwrap(), b"old-a");
            assert_eq!(std::fs::read(libs.join("b.jar")).unwrap(), b"old-b");
            assert!(!cache.journal.exists());
            assert!(!cache.journal_previous.exists());
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn journal_primary_failure_after_commit_keeps_the_new_outputs() {
        let _guard = TX_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for stage in ["write", "sync", "rename"] {
            let root = test_root(&format!("journal_primary_failure_{stage}"));
            let (cache, libs, specs) = transaction_setup(&root);
            set_test_journal_write_failure(Some((2, "primary", stage)));
            let result = cache.publish_outputs(&libs, &specs);
            set_test_journal_write_failure(None);
            assert!(
                result.is_ok(),
                "primary {stage} failure must be non-fatal: {result:?}"
            );
            assert_eq!(
                std::fs::read(libs.join("a.jar")).unwrap(),
                test_jar("new-a")
            );
            assert_eq!(
                std::fs::read(libs.join("b.jar")).unwrap(),
                test_jar("new-b")
            );
            assert!(!cache.journal.exists());
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn journal_primary_failure_leaves_the_secondary_generation_selectable() {
        let root = test_root("journal_primary_select");
        let cache = test_cache(&root);
        cache.prepare().unwrap();
        let libs = root.join("libraries");
        let mut journal = test_journal(vec![journal_entry(
            libs.join("a.jar"),
            cache.staging_dir.join("0.jar"),
            cache.backup_dir.join("0.bak"),
        )]);
        journal.generation = 1;
        set_test_journal_write_failure(Some((1, "primary", "write")));
        cache.write_journal(&mut journal).unwrap();
        set_test_journal_write_failure(None);
        let loaded = cache.load_journal().unwrap().unwrap();
        assert_eq!(loaded.generation, 1);
        assert_eq!(loaded.stage, JournalStage::Prepared);
        assert!(
            !cache.journal.exists(),
            "primary write was injected to fail and the stale primary must be removed"
        );
        assert!(cache.journal_previous.exists());
        // If the only remaining copy is later damaged, fail closed instead of
        // selecting a stale older primary.
        write_file(&cache.journal_previous, b"{corrupt");
        assert!(cache.load_journal().is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn journal_secondary_failure_keeps_the_previous_generation() {
        let root = test_root("journal_previous_generation");
        let cache = test_cache(&root);
        cache.prepare().unwrap();
        let libs = root.join("libraries");
        let old = test_journal(vec![journal_entry(
            libs.join("a.jar"),
            cache.staging_dir.join("0.jar"),
            cache.backup_dir.join("0.bak"),
        )]);
        write_journal_files(&cache, &old);
        let mut newer = old.clone();
        newer.generation = 2;
        newer.seal().unwrap();
        set_test_journal_write_failure(Some((2, "secondary", "write")));
        assert!(cache.write_journal(&mut newer).is_err());
        set_test_journal_write_failure(None);
        let loaded = cache.load_journal().unwrap().unwrap();
        assert_eq!(loaded.generation, 1);
        assert_eq!(loaded.tx_id, old.tx_id);
        assert!(
            !cache.journal.exists(),
            "the old primary must have been invalidated before the failed secondary write"
        );
        assert!(cache.journal_previous.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn journal_temp_corruption_uses_the_other_copy() {
        let root = test_root("journal_temp_corrupt");
        let cache = test_cache(&root);
        cache.prepare().unwrap();
        let libs = root.join("libraries");
        let mut journal = test_journal(vec![journal_entry(
            libs.join("a.jar"),
            cache.staging_dir.join("0.jar"),
            cache.backup_dir.join("0.bak"),
        )]);
        journal.generation = 1;
        set_test_journal_temp_corrupt(Some((1, "secondary")));
        cache.write_journal(&mut journal).unwrap();
        set_test_journal_temp_corrupt(None);
        let loaded = cache.load_journal().unwrap().unwrap();
        assert_eq!(loaded.generation, 1);
        assert!(cache.journal.exists());
        assert!(cache.journal_previous.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn prepared_rollback_with_a_missing_backup_changes_no_targets() {
        let _guard = TX_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = test_root("prepared_missing_backup");
        let status = spawn_tx_child(&root, "publish", "after_publish_1");
        assert!(!status.success());
        let cache = test_cache(&root);
        let libs = root.join("libraries");
        std::fs::remove_file(cache.backup_dir.join("0.bak")).unwrap();

        assert!(cache.recover_pending_transaction(&libs).is_err());
        assert_eq!(
            std::fs::read(libs.join("a.jar")).unwrap(),
            test_jar("new-a")
        );
        assert_eq!(
            std::fs::read(libs.join("b.jar")).unwrap(),
            test_jar("new-b")
        );
        assert!(cache.backup_dir.join("1.bak").is_file());
        assert!(cache.journal.exists());

        assert!(cache.recover_pending_transaction(&libs).is_err());
        assert_eq!(
            std::fs::read(libs.join("a.jar")).unwrap(),
            test_jar("new-a")
        );
        assert_eq!(
            std::fs::read(libs.join("b.jar")).unwrap(),
            test_jar("new-b")
        );
        assert!(cache.backup_dir.join("1.bak").is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn committed_partial_backup_cleanup_crash_requires_repair_without_mixed_set() {
        let _guard = TX_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = test_root("committed_partial_cleanup");
        let status = spawn_tx_child(&root, "publish", "cleanup_after_first_backup_removed");
        assert!(!status.success());
        let cache = test_cache(&root);
        let libs = root.join("libraries");
        let missing_backup = [0usize, 1]
            .into_iter()
            .find(|index| !cache.backup_dir.join(format!("{index}.bak")).exists())
            .expect("cleanup crash must remove exactly one backup");
        let surviving_index = 1 - missing_backup;
        let damaged_target = libs.join(if missing_backup == 0 {
            "a.jar"
        } else {
            "b.jar"
        });
        let surviving_target = libs.join(if surviving_index == 0 {
            "a.jar"
        } else {
            "b.jar"
        });
        std::fs::remove_file(&damaged_target).unwrap();
        let before_surviving = std::fs::read(&surviving_target).unwrap();

        assert!(cache.recover_pending_transaction(&libs).is_err());
        assert!(
            !damaged_target.exists(),
            "committed recovery must not guess an old target"
        );
        assert_eq!(std::fs::read(&surviving_target).unwrap(), before_surviving);
        assert!(cache
            .backup_dir
            .join(format!("{surviving_index}.bak"))
            .is_file());
        assert!(!cache
            .backup_dir
            .join(format!("{missing_backup}.bak"))
            .exists());
        assert!(cache.journal.exists());

        assert!(cache.recover_pending_transaction(&libs).is_err());
        assert!(!damaged_target.exists());
        assert_eq!(std::fs::read(&surviving_target).unwrap(), before_surviving);
        assert!(cache
            .backup_dir
            .join(format!("{surviving_index}.bak"))
            .is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn journal_missing_authoritative_secondary_refuses_to_advance() {
        let root = test_root("journal_missing_authoritative");
        let cache = test_cache(&root);
        cache.prepare().unwrap();
        let libs = root.join("libraries");
        let old = test_journal(vec![journal_entry(
            libs.join("a.jar"),
            cache.staging_dir.join("0.jar"),
            cache.backup_dir.join("0.bak"),
        )]);
        write_journal_files(&cache, &old);
        std::fs::remove_file(&cache.journal_previous).unwrap();
        let mut newer = old.clone();
        newer.generation = 2;
        newer.seal().unwrap();

        assert!(cache.write_journal(&mut newer).is_err());
        assert!(
            cache.journal.exists(),
            "the last valid journal must not be invalidated when the authoritative copy is missing"
        );
        let loaded = cache.load_journal().unwrap().unwrap();
        assert_eq!(loaded.generation, 1);
        assert_eq!(loaded.tx_id, old.tx_id);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn journal_mirror_invalidation_failure_does_not_advance_generation() {
        let root = test_root("journal_mirror_invalidation_failure");
        let cache = test_cache(&root);
        cache.prepare().unwrap();
        let libs = root.join("libraries");
        let old = test_journal(vec![journal_entry(
            libs.join("a.jar"),
            cache.staging_dir.join("0.jar"),
            cache.backup_dir.join("0.bak"),
        )]);
        write_journal_files(&cache, &old);
        // Force a real filesystem removal failure: `remove_file` cannot remove
        // a directory on Windows or Unix.
        std::fs::remove_file(&cache.journal).unwrap();
        std::fs::create_dir(&cache.journal).unwrap();
        let old_tx_id = old.tx_id.clone();
        let mut newer = old.clone();
        newer.generation = 2;
        newer.seal().unwrap();

        assert!(cache.write_journal(&mut newer).is_err());

        let loaded = cache.load_journal().unwrap().unwrap();
        assert_eq!(loaded.generation, 1);
        assert_eq!(loaded.tx_id, old_tx_id);
        assert!(
            cache.journal.is_dir(),
            "invalidation failure must keep the old primary entry"
        );
        assert!(cache.journal_previous.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn crash_before_commit_mirror_invalidation_keeps_the_old_generation() {
        let _guard = TX_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = test_root("journal_crash_before_invalidation");
        let status = spawn_tx_child(&root, "publish", "before_mirror_invalidation_2");
        assert!(!status.success());
        let cache = test_cache(&root);
        let libs = root.join("libraries");
        let journal = cache.load_journal().unwrap().unwrap();
        assert_eq!(journal.generation, 1);
        assert_eq!(journal.stage, JournalStage::Prepared);
        assert!(cache.journal.exists());
        assert!(cache.journal_previous.exists());
        assert_eq!(
            std::fs::read(libs.join("a.jar")).unwrap(),
            test_jar("new-a")
        );
        assert_eq!(
            std::fs::read(libs.join("b.jar")).unwrap(),
            test_jar("new-b")
        );

        cache.recover_pending_transaction(&libs).unwrap();
        assert_eq!(std::fs::read(libs.join("a.jar")).unwrap(), b"old-a");
        assert_eq!(std::fs::read(libs.join("b.jar")).unwrap(), b"old-b");
        assert!(!cache.journal.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn crash_after_commit_mirror_invalidation_keeps_the_old_generation() {
        let _guard = TX_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = test_root("journal_crash_after_invalidation");
        let status = spawn_tx_child(&root, "publish", "after_mirror_invalidation_2");
        assert!(!status.success());
        let cache = test_cache(&root);
        let libs = root.join("libraries");
        let journal = cache.load_journal().unwrap().unwrap();
        assert_eq!(journal.generation, 1);
        assert_eq!(journal.stage, JournalStage::Prepared);
        assert!(
            !cache.journal.exists(),
            "old primary was invalidated before the crash"
        );
        assert!(cache.journal_previous.exists());
        assert_eq!(
            std::fs::read(libs.join("a.jar")).unwrap(),
            test_jar("new-a")
        );
        assert_eq!(
            std::fs::read(libs.join("b.jar")).unwrap(),
            test_jar("new-b")
        );

        cache.recover_pending_transaction(&libs).unwrap();
        assert_eq!(std::fs::read(libs.join("a.jar")).unwrap(), b"old-a");
        assert_eq!(std::fs::read(libs.join("b.jar")).unwrap(), b"old-b");
        assert!(!cache.journal.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn new_secondary_corruption_without_a_mirror_fails_closed() {
        let _guard = TX_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = test_root("journal_new_secondary_corrupt");
        let status = spawn_tx_child(&root, "publish", "after_secondary_committed_journal");
        assert!(!status.success());
        let cache = test_cache(&root);
        let libs = root.join("libraries");
        assert!(!cache.journal.exists());
        assert!(cache.journal_previous.exists());
        let before_a = std::fs::read(libs.join("a.jar")).unwrap();
        let before_b = std::fs::read(libs.join("b.jar")).unwrap();

        write_file(&cache.journal_previous, b"{corrupt-new-secondary");
        assert!(cache.load_journal().is_err());
        assert!(cache.recover_pending_transaction(&libs).is_err());
        assert_eq!(std::fs::read(libs.join("a.jar")).unwrap(), before_a);
        assert_eq!(std::fs::read(libs.join("b.jar")).unwrap(), before_b);
        assert!(cache.recover_pending_transaction(&libs).is_err());
        assert_eq!(std::fs::read(libs.join("a.jar")).unwrap(), before_a);
        assert_eq!(std::fs::read(libs.join("b.jar")).unwrap(), before_b);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn loader_preparation_path_reports_recovery_required_without_deleting_backups() {
        let root = test_root("repair_recovery_required");
        let status = spawn_tx_child(&root, "publish", "after_commit");
        assert!(!status.success());
        let cache = test_cache(&root);
        let libs = root.join("libraries");
        let versions = root.join("versions");
        std::fs::create_dir_all(&versions).unwrap();
        std::fs::remove_file(libs.join("a.jar")).unwrap();

        // This is the same preparation call made by the loader install/repair
        // path before it can regenerate processor outputs.
        let error = crate::modloaders::forge::ensure_processor_jars(
            "1.20.1", "47.4.29", &libs, &versions, None,
        )
        .await
        .expect_err("Repair/launch preparation must not bypass Recovery Required");
        assert!(
            error.to_string().contains("Committed processor generation"),
            "unexpected repair-path error: {error}"
        );
        assert!(!libs.join("a.jar").exists());
        assert_eq!(
            std::fs::read(libs.join("b.jar")).unwrap(),
            test_jar("new-b")
        );
        assert!(cache.backup_dir.join("0.bak").is_file());
        assert!(cache.backup_dir.join("1.bak").is_file());
        assert!(cache.journal.exists());
        assert!(cache.journal_previous.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn malformed_installer_identity_checksums_are_stale_not_fatal() {
        let root = test_root("installer_identity_malformed");
        let cache = test_cache(&root);
        cache.prepare().unwrap();
        let identity = installer(&cache, b"installer-bytes");

        let mut bad_algorithm = identity.clone();
        bad_algorithm.checksum_algorithm = "not-a-hash".to_string();
        cache.write_installer_identity(&bad_algorithm).unwrap();
        assert!(
            !cache
                .installer_identity_matches(&bad_algorithm)
                .expect("malformed algorithm must be treated as stale metadata"),
            "malformed algorithm must not be accepted as a trusted identity"
        );

        let mut bad_hex = identity.clone();
        bad_hex.checksum_hex = "zz".to_string();
        cache.write_installer_identity(&bad_hex).unwrap();
        assert!(
            !cache
                .installer_identity_matches(&bad_hex)
                .expect("malformed hex must be treated as stale metadata"),
            "malformed checksum hex must not be accepted as a trusted identity"
        );

        // A read failure is not metadata corruption and must remain an error.
        std::fs::remove_file(&cache.installer).unwrap();
        assert!(
            !cache
                .installer_identity_matches(&identity)
                .expect("a missing installer is simply not a match"),
            "a missing installer cannot match an identity"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn installer_publication_runs_under_cache_operation_lock() {
        let root = test_root("installer_lock");
        let cache = test_cache(&root);
        let (lock_held_tx, lock_held_rx) = std::sync::mpsc::channel();
        let lock_held_tx = StdMutex::new(lock_held_tx);
        cache.set_test_installer_hook(Some(Arc::new(move |cache, stage| {
            if stage == InstallerTestStage::AfterLock {
                let held = cache.try_acquire_operation_lock().is_none();
                lock_held_tx.lock().unwrap().send(held).unwrap();
            }
        })));

        let result = cache
            .ensure_installer(
                &[],
                crate::modloaders::artifact::FORGE_HOSTS,
                "test installer",
            )
            .await;
        cache.set_test_installer_hook(None);
        assert!(result.is_err(), "empty candidates must fail after locking");
        assert!(
            lock_held_rx.recv().expect("installer hook must run"),
            "installer publication must hold the cache operation lock"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn malformed_installer_identity_is_reverified_and_replaced() {
        let _proxy = direct_proxy_lock().await;
        let root = test_root("installer_identity_reverify");
        let cache = test_cache(&root);
        cache.prepare().unwrap();
        let old_identity = installer(&cache, b"old-installer-bytes");
        let mut malformed = old_identity;
        malformed.checksum_algorithm = "invalid-algorithm".to_string();
        cache.write_installer_identity(&malformed).unwrap();

        let body = b"fresh-installer-bytes";
        let hash = sha512_hex(body);
        let server = spawn_mock(vec![MockSpec::ok(hash.as_bytes()), MockSpec::ok(body)]).await;
        let base = server.url.trim_end_matches("artifact.bin").to_string();
        let candidate = format!("{base}installer.jar");
        cache
            .ensure_installer(
                &[candidate],
                crate::modloaders::artifact::FORGE_HOSTS,
                "test installer",
            )
            .await
            .expect("malformed local metadata must trigger a verified re-download");
        assert_eq!(std::fs::read(&cache.installer).unwrap(), body);
        let reloaded = cache.load_installer_identity().unwrap().unwrap();
        assert!(cache.installer_identity_matches(&reloaded).unwrap());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn inspect_transaction_distinguishes_prepared_committed_and_damaged() {
        let root = test_root("inspect_transaction_states");
        let cache = test_cache(&root);
        cache.prepare().unwrap();
        let libs = root.join("libraries");
        std::fs::create_dir_all(&libs).unwrap();

        let entry = journal_entry(
            libs.join("a.jar"),
            cache.staging_dir.join("0.jar"),
            cache.backup_dir.join("0.bak"),
        );
        let prepared = test_journal(vec![entry.clone()]);
        write_journal_files(&cache, &prepared);
        let inspection = cache.inspect_transaction(&libs).unwrap();
        assert_eq!(inspection.state, TransactionState::Prepared);

        let bytes = test_jar("new-a");
        write_file(&libs.join("a.jar"), &bytes);
        let digest = file_digest(&libs.join("a.jar")).unwrap();
        let mut committed_entry = entry;
        committed_entry.new_sha256 = digest.sha256;
        let mut committed = test_journal(vec![committed_entry]);
        committed.stage = JournalStage::Committed;
        committed.seal().unwrap();
        write_journal_files(&cache, &committed);
        assert_eq!(
            cache.inspect_transaction(&libs).unwrap().state,
            TransactionState::CommittedValid
        );

        std::fs::remove_file(libs.join("a.jar")).unwrap();
        assert_eq!(
            cache.inspect_transaction(&libs).unwrap().state,
            TransactionState::CommittedDamaged
        );

        std::fs::write(&cache.journal, b"{corrupt").unwrap();
        std::fs::write(&cache.journal_previous, b"{corrupt").unwrap();
        assert!(cache.inspect_transaction(&libs).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}

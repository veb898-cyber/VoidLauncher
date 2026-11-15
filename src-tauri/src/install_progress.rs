//! Aggregated, monotonic install progress.
//!
//! Minecraft installs download many files in parallel and add new work in
//! stages (libraries first, then assets after the asset index is parsed).
//! A single cumulative tracker is the source of truth:
//!
//! - `prepared` counts only files that passed their final integrity check
//!   (including valid cache hits);
//! - `in_progress` counts the current partial bytes of files that are still
//!   downloading and have not been verified yet;
//! - parallel files update concurrently without double-counting retries;
//! - the percentage stays indeterminate until the whole plan is known and
//!   every planned file has a trustworthy size;
//! - only `done()` may produce 100%.

use crate::events::{InstallProgressPayload, ProgressSender};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const DEFAULT_EMIT_INTERVAL: Duration = Duration::from_millis(200);

#[derive(Debug, Clone)]
pub struct PlannedFile {
    pub key: String,
    pub total: Option<u64>,
    /// Bytes already verified for this file (valid cache).
    pub prepared: u64,
    /// Network bytes already transferred for this operation.
    pub network: u64,
    pub completed: bool,
}

impl PlannedFile {
    pub fn missing(key: impl Into<String>, size: Option<u64>) -> Self {
        Self {
            key: key.into(),
            total: size,
            prepared: 0,
            network: 0,
            completed: false,
        }
    }

    #[cfg(test)]
    pub fn cached(key: impl Into<String>, size: u64) -> Self {
        Self {
            key: key.into(),
            total: Some(size),
            prepared: size,
            network: 0,
            completed: true,
        }
    }
}

/// Stable key for a download target. Windows paths are case-insensitive.
pub fn file_key(path: &std::path::Path) -> String {
    path.to_string_lossy().to_ascii_lowercase()
}

fn planned_files_from_downloads(
    files: &[(String, std::path::PathBuf, String, u64)],
) -> Vec<PlannedFile> {
    files
        .iter()
        .map(|(_, path, _, size)| {
            PlannedFile::missing(file_key(path), if *size > 0 { Some(*size) } else { None })
        })
        .collect()
}

/// Convenience for callers that already hold the download plan.
pub fn planned_files(files: &[(String, std::path::PathBuf, String, u64)]) -> Vec<PlannedFile> {
    planned_files_from_downloads(files)
}

#[derive(Debug)]
struct FileState {
    total: Option<u64>,
    /// Fully verified bytes. Mutually exclusive with `in_progress`.
    prepared: u64,
    /// Current partial bytes of an unverified download attempt.
    in_progress: u64,
    network: u64,
    completed: bool,
}

struct Inner {
    install_id: String,
    instance_id: String,
    stage: String,
    message: String,
    files: HashMap<String, FileState>,
    plan_complete: bool,
    done: bool,
    failed: bool,
    last_emit: Instant,
    emit_interval: Duration,
}

#[derive(Clone)]
pub struct InstallProgressTracker {
    inner: Arc<Mutex<Inner>>,
    emitter: Arc<dyn Fn(InstallProgressPayload) + Send + Sync>,
}

impl InstallProgressTracker {
    pub fn new(install_id: String, instance_id: String, sender: ProgressSender) -> Self {
        Self::with_emitter(
            install_id,
            instance_id,
            Arc::new(move |payload| sender.send(payload)),
        )
    }

    pub fn with_emitter(
        install_id: String,
        instance_id: String,
        emitter: Arc<dyn Fn(InstallProgressPayload) + Send + Sync>,
    ) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                install_id,
                instance_id,
                stage: "manifest".to_string(),
                message: String::new(),
                files: HashMap::new(),
                plan_complete: false,
                done: false,
                failed: false,
                last_emit: Instant::now() - DEFAULT_EMIT_INTERVAL,
                emit_interval: DEFAULT_EMIT_INTERVAL,
            })),
            emitter,
        }
    }

    #[cfg(test)]
    pub fn set_emit_interval(&self, interval: Duration) {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .emit_interval = interval;
    }

    pub fn set_stage(&self, stage: &str, message: &str) {
        {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            inner.stage = stage.to_string();
            inner.message = message.to_string();
        }
        self.emit(true);
    }

    pub fn add_files(&self, stage: &str, message: &str, files: Vec<PlannedFile>) {
        {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            inner.stage = stage.to_string();
            inner.message = message.to_string();
            // New work means the total is provisional again until the caller
            // explicitly confirms the plan is complete.
            inner.plan_complete = false;
            for file in files {
                let entry = inner.files.entry(file.key).or_insert_with(|| FileState {
                    total: file.total,
                    prepared: file.prepared,
                    in_progress: 0,
                    network: file.network,
                    completed: file.completed,
                });
                if entry.total.is_none() {
                    entry.total = file.total;
                }
                if file.completed {
                    entry.prepared = entry.prepared.max(file.prepared);
                    entry.in_progress = 0;
                    entry.completed = true;
                }
                if let Some(total) = entry.total {
                    entry.prepared = entry.prepared.min(total);
                    entry.in_progress = entry.in_progress.min(total.saturating_sub(entry.prepared));
                }
            }
        }
        self.emit(true);
    }

    pub fn mark_plan_complete(&self) {
        {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            inner.plan_complete = true;
        }
        self.emit(true);
    }

    /// Update one file's current partial bytes and add newly transferred
    /// network bytes. `downloaded` is the current `.part` size of this attempt:
    /// a retry that restarts from zero lowers `in_progress`; it never touches
    /// `prepared` because the file has not passed its final verification yet.
    /// Network bytes are cumulative for the operation and never double-count a
    /// reported delta.
    pub fn file_progress(
        &self,
        key: &str,
        downloaded: u64,
        total_hint: Option<u64>,
        network_delta: u64,
    ) {
        {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let entry = inner
                .files
                .entry(key.to_string())
                .or_insert_with(|| FileState {
                    total: total_hint.filter(|size| *size > 0),
                    prepared: 0,
                    in_progress: 0,
                    network: 0,
                    completed: false,
                });
            if entry.total.is_none() {
                entry.total = total_hint.filter(|size| *size > 0);
            }
            let target = entry.total.unwrap_or(u64::MAX);
            // A late event from an older attempt must not move a verified
            // file back into the unverified bucket.
            if !entry.completed {
                entry.in_progress = downloaded.min(target);
            }
            entry.network = entry.network.saturating_add(network_delta);
        }
        self.emit(false);
    }

    /// A file is complete only after the downloader has verified it (or a
    /// cache check proved it valid). `actual_size` is the on-disk length.
    pub fn file_completed(&self, key: &str, actual_size: u64) {
        {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let entry = inner
                .files
                .entry(key.to_string())
                .or_insert_with(|| FileState {
                    total: Some(actual_size),
                    prepared: actual_size,
                    in_progress: 0,
                    network: 0,
                    completed: true,
                });
            entry.total = Some(actual_size);
            entry.prepared = actual_size;
            entry.in_progress = 0;
            entry.completed = true;
        }
        self.emit(false);
    }

    /// Terminal error/cancel. Never turns into 100%.
    pub fn fail(&self, stage: &str, message: &str) {
        {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            inner.stage = stage.to_string();
            inner.message = message.to_string();
            inner.failed = true;
        }
        self.emit(true);
    }

    pub fn done(&self) {
        {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            inner.stage = "done".to_string();
            inner.message = "Installation complete".to_string();
            inner.done = true;
            inner.plan_complete = true;
        }
        self.emit(true);
    }

    #[cfg(test)]
    pub fn snapshot(&self) -> InstallProgressPayload {
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Self::snapshot_locked(&inner)
    }

    fn snapshot_locked(inner: &Inner) -> InstallProgressPayload {
        let mut prepared = 0u64;
        let mut in_progress = 0u64;
        let mut network = 0u64;
        let mut all_sizes_known = !inner.files.is_empty();
        let mut total = 0u64;
        for file in inner.files.values() {
            prepared = prepared.saturating_add(file.prepared);
            in_progress = in_progress.saturating_add(file.in_progress);
            network = network.saturating_add(file.network);
            match file.total {
                Some(size) => total = total.saturating_add(size),
                None => all_sizes_known = false,
            }
        }

        let determinate = inner.plan_complete && all_sizes_known;
        let done = inner.done;
        let failed = inner.failed;
        let cap = if determinate && total > 0 {
            total
        } else {
            u64::MAX
        };
        let prepared_bytes = prepared.min(cap);
        // `in_progress` and `prepared` are mutually exclusive per file; the
        // remaining cap is only a final guard against a stale event.
        let in_progress_bytes = in_progress.min(cap.saturating_sub(prepared_bytes));
        let accounted = prepared_bytes.saturating_add(in_progress_bytes);
        let percent = if done {
            100.0
        } else if failed {
            0.0
        } else if determinate && total > 0 {
            ((accounted.min(total) as f64 / total as f64) * 100.0).min(99.0)
        } else {
            0.0
        };
        let indeterminate = !done && (!determinate || failed);

        InstallProgressPayload {
            install_id: inner.install_id.clone(),
            instance_id: inner.instance_id.clone(),
            percent,
            prepared_bytes,
            in_progress_bytes,
            network_downloaded_bytes: network,
            total_bytes: if determinate { total } else { 0 },
            indeterminate,
            stage: inner.stage.clone(),
            message: inner.message.clone(),
        }
    }

    fn emit(&self, force: bool) {
        let payload = {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if !force && inner.last_emit.elapsed() < inner.emit_interval {
                return;
            }
            inner.last_emit = Instant::now();
            Self::snapshot_locked(&inner)
        };
        (self.emitter)(payload);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn tracker(
        id: &str,
    ) -> (
        InstallProgressTracker,
        Arc<Mutex<Vec<InstallProgressPayload>>>,
    ) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = events.clone();
        let tracker = InstallProgressTracker::with_emitter(
            id.to_string(),
            "instance".to_string(),
            Arc::new(move |payload| {
                sink.lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push(payload);
            }),
        );
        tracker.set_emit_interval(Duration::ZERO);
        (tracker, events)
    }

    fn plan_one(tracker: &InstallProgressTracker, key: &str, size: u64) {
        tracker.add_files(
            "libraries",
            "Downloading libraries...",
            vec![PlannedFile::missing(key, Some(size))],
        );
        tracker.mark_plan_complete();
    }

    fn assert_percent(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 0.01,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn network_chunks_fill_in_progress_without_touching_prepared() {
        let (tracker, _) = tracker("install-1");
        plan_one(&tracker, "a.jar", 100);
        tracker.file_progress("a.jar", 40, Some(100), 40);
        let snapshot = tracker.snapshot();
        assert_eq!(snapshot.prepared_bytes, 0);
        assert_eq!(snapshot.in_progress_bytes, 40);
        assert_eq!(snapshot.network_downloaded_bytes, 40);
        assert_eq!(snapshot.total_bytes, 100);
        assert!(!snapshot.indeterminate);
        assert_percent(snapshot.percent, 40.0);

        tracker.file_progress("a.jar", 70, Some(100), 30);
        let snapshot = tracker.snapshot();
        assert_eq!(snapshot.prepared_bytes, 0);
        assert_eq!(snapshot.in_progress_bytes, 70);
        assert_eq!(snapshot.network_downloaded_bytes, 70);
        assert_percent(snapshot.percent, 70.0);
    }

    #[test]
    fn completion_moves_verified_bytes_from_in_progress_to_prepared() {
        let (tracker, _) = tracker("install-1");
        plan_one(&tracker, "a.jar", 100);
        tracker.file_progress("a.jar", 60, Some(100), 60);
        assert_eq!(tracker.snapshot().prepared_bytes, 0);
        assert_eq!(tracker.snapshot().in_progress_bytes, 60);

        tracker.file_completed("a.jar", 100);
        let snapshot = tracker.snapshot();
        assert_eq!(snapshot.prepared_bytes, 100);
        assert_eq!(snapshot.in_progress_bytes, 0);
        assert_eq!(snapshot.network_downloaded_bytes, 60);
        assert_eq!(snapshot.total_bytes, 100);
        assert!(!snapshot.indeterminate);
        assert_percent(snapshot.percent, 99.0);
    }

    #[test]
    fn prepared_and_in_progress_never_double_count_the_same_file() {
        let (tracker, _) = tracker("install-1");
        plan_one(&tracker, "a.jar", 100);
        tracker.file_progress("a.jar", 100, Some(100), 100);
        let snapshot = tracker.snapshot();
        assert_eq!(snapshot.prepared_bytes, 0);
        assert_eq!(snapshot.in_progress_bytes, 100);
        assert_eq!(snapshot.prepared_bytes + snapshot.in_progress_bytes, 100);

        tracker.file_completed("a.jar", 100);
        let snapshot = tracker.snapshot();
        assert_eq!(snapshot.prepared_bytes, 100);
        assert_eq!(snapshot.in_progress_bytes, 0);
        assert_eq!(snapshot.prepared_bytes + snapshot.in_progress_bytes, 100);
    }

    #[test]
    fn sequential_files_keep_prepared_and_in_progress_separate() {
        let (tracker, _) = tracker("install-1");
        tracker.add_files(
            "libraries",
            "Downloading libraries...",
            vec![
                PlannedFile::missing("a.jar", Some(100)),
                PlannedFile::missing("b.jar", Some(50)),
            ],
        );
        tracker.mark_plan_complete();
        tracker.file_completed("a.jar", 100);
        assert_eq!(tracker.snapshot().prepared_bytes, 100);
        assert_eq!(tracker.snapshot().in_progress_bytes, 0);

        tracker.file_progress("b.jar", 25, Some(50), 25);
        let snapshot = tracker.snapshot();
        assert_eq!(snapshot.prepared_bytes, 100);
        assert_eq!(snapshot.in_progress_bytes, 25);
        assert_eq!(snapshot.network_downloaded_bytes, 25);
        assert_eq!(snapshot.total_bytes, 150);
        assert_percent(snapshot.percent, (125.0 / 150.0) * 100.0);
    }

    #[test]
    fn parallel_files_sum_partial_contributions_and_move_one_bucket_on_completion() {
        let (tracker, _) = tracker("install-1");
        tracker.add_files(
            "libraries",
            "Downloading libraries...",
            vec![
                PlannedFile::missing("a.jar", Some(100)),
                PlannedFile::missing("b.jar", Some(200)),
            ],
        );
        tracker.mark_plan_complete();
        tracker.file_progress("a.jar", 30, Some(100), 30);
        tracker.file_progress("b.jar", 80, Some(200), 80);
        tracker.file_progress("a.jar", 60, Some(100), 30);
        tracker.file_progress("b.jar", 120, Some(200), 40);
        let snapshot = tracker.snapshot();
        assert_eq!(snapshot.prepared_bytes, 0);
        assert_eq!(snapshot.in_progress_bytes, 180);
        assert_eq!(snapshot.network_downloaded_bytes, 180);
        assert_percent(snapshot.percent, 60.0);

        tracker.file_completed("a.jar", 100);
        let snapshot = tracker.snapshot();
        assert_eq!(snapshot.prepared_bytes, 100);
        assert_eq!(snapshot.in_progress_bytes, 120);
        assert_eq!(snapshot.network_downloaded_bytes, 180);
        assert_percent(snapshot.percent, (220.0 / 300.0) * 100.0);
    }

    #[test]
    fn partial_bytes_are_capped_by_the_planned_file_size() {
        let (tracker, _) = tracker("install-1");
        plan_one(&tracker, "a.jar", 100);
        tracker.file_progress("a.jar", 150, Some(100), 120);
        let snapshot = tracker.snapshot();
        assert_eq!(snapshot.prepared_bytes, 0);
        assert_eq!(snapshot.in_progress_bytes, 100);
        assert_eq!(snapshot.network_downloaded_bytes, 120);
        assert_percent(snapshot.percent, 99.0);
    }

    #[test]
    fn retry_resets_in_progress_and_keeps_prepared_and_network() {
        let (tracker, _) = tracker("install-1");
        tracker.add_files(
            "libraries",
            "Downloading libraries...",
            vec![
                PlannedFile::missing("done.jar", Some(50)),
                PlannedFile::missing("a.jar", Some(100)),
            ],
        );
        tracker.mark_plan_complete();
        tracker.file_completed("done.jar", 50);

        tracker.file_progress("a.jar", 30, Some(100), 30);
        let snapshot = tracker.snapshot();
        assert_eq!(snapshot.prepared_bytes, 50);
        assert_eq!(snapshot.in_progress_bytes, 30);
        assert_eq!(snapshot.network_downloaded_bytes, 30);

        tracker.file_progress("a.jar", 10, Some(100), 10); // retry from zero
        let snapshot = tracker.snapshot();
        assert_eq!(snapshot.prepared_bytes, 50);
        assert_eq!(snapshot.in_progress_bytes, 10);
        assert_eq!(snapshot.network_downloaded_bytes, 40);

        tracker.file_progress("a.jar", 40, Some(100), 30);
        let snapshot = tracker.snapshot();
        assert_eq!(snapshot.prepared_bytes, 50);
        assert_eq!(snapshot.in_progress_bytes, 40);
        assert_eq!(snapshot.network_downloaded_bytes, 70);
    }

    #[test]
    fn failure_never_promotes_partial_bytes_to_prepared() {
        let (tracker, _) = tracker("install-1");
        plan_one(&tracker, "a.jar", 100);
        tracker.file_progress("a.jar", 30, Some(100), 30);
        tracker.fail("error", "download failed");
        let snapshot = tracker.snapshot();
        assert_eq!(snapshot.stage, "error");
        assert_eq!(snapshot.prepared_bytes, 0);
        assert_eq!(snapshot.in_progress_bytes, 30);
        assert_eq!(snapshot.network_downloaded_bytes, 30);
        assert!(snapshot.indeterminate);
        assert_eq!(snapshot.percent, 0.0);
        assert_ne!(snapshot.stage, "done");
    }

    #[test]
    fn cached_files_count_as_prepared_without_network_or_in_progress() {
        let (tracker, _) = tracker("install-1");
        tracker.add_files(
            "libraries",
            "Downloading libraries...",
            vec![
                PlannedFile::cached("cached.jar", 120),
                PlannedFile::missing("missing.jar", Some(80)),
            ],
        );
        tracker.mark_plan_complete();
        let snapshot = tracker.snapshot();
        assert_eq!(snapshot.prepared_bytes, 120);
        assert_eq!(snapshot.in_progress_bytes, 0);
        assert_eq!(snapshot.network_downloaded_bytes, 0);
        assert_eq!(snapshot.total_bytes, 200);
        assert!(!snapshot.indeterminate);
    }

    #[test]
    fn unknown_size_partial_stays_in_progress_until_completion() {
        let (tracker, _) = tracker("install-1");
        tracker.add_files(
            "libraries",
            "Downloading libraries...",
            vec![PlannedFile::missing("unknown.jar", None)],
        );
        tracker.mark_plan_complete();
        tracker.file_progress("unknown.jar", 42, None, 42);
        let snapshot = tracker.snapshot();
        assert!(snapshot.indeterminate);
        assert_eq!(snapshot.total_bytes, 0);
        assert_eq!(snapshot.percent, 0.0);
        assert_eq!(snapshot.prepared_bytes, 0);
        assert_eq!(snapshot.in_progress_bytes, 42);
        assert_eq!(snapshot.network_downloaded_bytes, 42);

        tracker.file_completed("unknown.jar", 90);
        let snapshot = tracker.snapshot();
        assert!(!snapshot.indeterminate);
        assert_eq!(snapshot.total_bytes, 90);
        assert_eq!(snapshot.prepared_bytes, 90);
        assert_eq!(snapshot.in_progress_bytes, 0);
        assert_eq!(snapshot.network_downloaded_bytes, 42);
        assert_percent(snapshot.percent, 99.0);
    }

    #[test]
    fn asset_plan_extends_total_and_keeps_previous_prepared() {
        let (tracker, _) = tracker("install-1");
        tracker.add_files(
            "libraries",
            "Downloading libraries...",
            vec![PlannedFile::missing("lib.jar", Some(100))],
        );
        tracker.file_completed("lib.jar", 100);
        tracker.set_stage("assets", "Downloading asset index...");
        assert!(tracker.snapshot().indeterminate);
        assert_eq!(tracker.snapshot().prepared_bytes, 100);
        assert_eq!(tracker.snapshot().in_progress_bytes, 0);

        tracker.add_files(
            "assets",
            "Downloading assets...",
            vec![
                PlannedFile::missing("asset-1", Some(50)),
                PlannedFile::missing("asset-2", Some(50)),
            ],
        );
        tracker.mark_plan_complete();
        tracker.file_progress("asset-1", 25, Some(50), 25);
        let snapshot = tracker.snapshot();
        assert_eq!(snapshot.prepared_bytes, 100);
        assert_eq!(snapshot.in_progress_bytes, 25);
        assert_eq!(snapshot.total_bytes, 200);
        assert!(!snapshot.indeterminate);
        assert_eq!(snapshot.stage, "assets");
        assert_percent(snapshot.percent, 62.5);
    }

    #[test]
    fn different_install_ids_are_isolated() {
        let (one, _) = tracker("install-1");
        let (two, _) = tracker("install-2");
        plan_one(&one, "a.jar", 100);
        plan_one(&two, "b.jar", 200);
        one.file_progress("a.jar", 50, Some(100), 50);
        two.file_completed("b.jar", 200);
        assert_eq!(one.snapshot().install_id, "install-1");
        assert_eq!(one.snapshot().prepared_bytes, 0);
        assert_eq!(one.snapshot().in_progress_bytes, 50);
        assert_eq!(one.snapshot().network_downloaded_bytes, 50);
        assert_eq!(two.snapshot().install_id, "install-2");
        assert_eq!(two.snapshot().prepared_bytes, 200);
        assert_eq!(two.snapshot().in_progress_bytes, 0);
    }

    #[test]
    fn stage_transition_keeps_prepared_and_in_progress() {
        let (tracker, _) = tracker("install-1");
        plan_one(&tracker, "lib.jar", 100);
        tracker.file_completed("lib.jar", 100);
        tracker.add_files(
            "assets",
            "Downloading assets...",
            vec![PlannedFile::missing("asset", Some(100))],
        );
        tracker.mark_plan_complete();
        tracker.set_stage("assets", "Downloading assets...");
        assert_eq!(tracker.snapshot().prepared_bytes, 100);
        tracker.file_progress("asset", 50, Some(100), 50);
        let snapshot = tracker.snapshot();
        assert_eq!(snapshot.prepared_bytes, 100);
        assert_eq!(snapshot.in_progress_bytes, 50);
        assert_eq!(snapshot.network_downloaded_bytes, 50);
        assert_eq!(snapshot.stage, "assets");
        assert_percent(snapshot.percent, 75.0);
    }

    #[test]
    fn only_done_emits_one_hundred_percent() {
        let (tracker, _) = tracker("install-1");
        plan_one(&tracker, "a.jar", 100);
        tracker.file_completed("a.jar", 100);
        assert_eq!(tracker.snapshot().percent, 99.0);
        tracker.done();
        assert_eq!(tracker.snapshot().percent, 100.0);
        assert!(!tracker.snapshot().indeterminate);
    }

    #[test]
    fn late_progress_after_completion_does_not_move_bytes_back() {
        let (tracker, _) = tracker("install-1");
        plan_one(&tracker, "a.jar", 100);
        tracker.file_completed("a.jar", 100);
        tracker.file_progress("a.jar", 30, Some(100), 0);
        let snapshot = tracker.snapshot();
        assert_eq!(snapshot.prepared_bytes, 100);
        assert_eq!(snapshot.in_progress_bytes, 0);
    }

    #[test]
    fn cancellation_is_isolated_and_not_complete() {
        let (one, _) = tracker("install-1");
        let (two, _) = tracker("install-2");
        plan_one(&one, "a.jar", 100);
        plan_one(&two, "b.jar", 100);
        one.fail("cancelled", "cancelled by user");
        assert_eq!(one.snapshot().stage, "cancelled");
        assert!(one.snapshot().percent < 100.0);
        assert_eq!(two.snapshot().stage, "libraries");
    }

    #[test]
    fn cached_and_invalid_plan_entries_are_explicit() {
        let cached = PlannedFile::cached("cached", 10);
        assert!(cached.completed);
        assert_eq!(cached.prepared, 10);
        assert_eq!(cached.network, 0);
        let missing = PlannedFile::missing("missing", None);
        assert!(!missing.completed);
        assert_eq!(missing.total, None);
    }
}

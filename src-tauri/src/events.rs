use serde::Serialize;
use std::sync::OnceLock;
use tauri::{AppHandle, Emitter};
use tokio::sync::broadcast;

/// Progress payload sent from backend → frontend
#[derive(Debug, Clone, Serialize)]
pub struct InstallProgressPayload {
    pub instance_id: String,
    pub percent: f64,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
    pub stage: String,
    pub message: String,
}

/// Launch state payload
#[derive(Debug, Clone, Serialize)]
pub struct LaunchEventPayload {
    pub instance_id: String,
    pub status: String,
    pub pid: Option<u32>,
    pub exit_code: Option<i32>,
}

/// Log message payload
#[derive(Debug, Clone, Serialize)]
pub struct LogPayload {
    pub level: String,
    pub source: String,
    pub message: String,
}

/// Global app handle used for fire-and-forget progress events from
/// modules that don't otherwise receive an AppHandle.
static APP_HANDLE: OnceLock<AppHandle> = OnceLock::new();

/// Store the app handle at startup so any module can emit Tauri events.
pub fn set_app_handle(app: &AppHandle) {
    let _ = APP_HANDLE.set(app.clone());
}

/// Retry notification while fetching a modpack catalog (ATL/CF/Modrinth).
#[derive(Debug, Clone, Serialize)]
pub struct FetchRetryPayload {
    pub source: String,
    pub attempt: usize,
    pub total: usize,
    pub message: String,
}

pub fn emit_fetch_retry(source: &str, attempt: usize, total: usize, message: &str) {
    if let Some(app) = APP_HANDLE.get() {
        let _ = app.emit(
            "modpack_fetch_retry",
            FetchRetryPayload {
                source: source.to_string(),
                attempt,
                total,
                message: message.to_string(),
            },
        );
    }
}

/// Byte-level progress of a single file download (throttled by caller).
#[derive(Debug, Clone, Serialize)]
pub struct FileProgressPayload {
    pub url: String,
    pub downloaded: u64,
    pub total: u64,
}

pub fn emit_file_progress(url: &str, downloaded: u64, total: u64) {
    if let Some(app) = APP_HANDLE.get() {
        let _ = app.emit(
            "modpack_file_progress",
            FileProgressPayload {
                url: url.to_string(),
                downloaded,
                total,
            },
        );
    }
}

/// A progress sender that wraps a broadcast::Sender
#[derive(Debug, Clone)]
pub struct ProgressSender {
    tx: broadcast::Sender<InstallProgressPayload>,
}

impl ProgressSender {
    pub fn new() -> (Self, broadcast::Receiver<InstallProgressPayload>) {
        let (tx, rx) = broadcast::channel(256);
        (Self { tx }, rx)
    }

    pub fn send(&self, payload: InstallProgressPayload) {
        let _ = self.tx.send(payload);
    }
}

/// Emit a log message event to the frontend AND write it to the
/// file-based tracing logger (see `logger::init`).
/// If the source is "launch", also append to the current game log file
/// so the Game Logs page captures launch messages.
pub fn emit_log(app: &AppHandle, level: &str, source: &str, message: &str) {
    // Mirror to the file logger. We do this BEFORE the IPC emit so a
    // crash inside the renderer (e.g. an exception in the Logs page)
    // doesn't lose the log line.
    match level {
        "error" => tracing::error!(target: "launcher", source = %source, "{}", message),
        "warn"  => tracing::warn!(target: "launcher", source = %source, "{}", message),
        "debug" => tracing::debug!(target: "launcher", source = %source, "{}", message),
        _       => tracing::info!(target: "launcher", source = %source, "{}", message),
    }

    let _ = app.emit(
        "log_message",
        LogPayload {
            level: level.to_string(),
            source: source.to_string(),
            message: message.to_string(),
        },
    );

    // Also write launch logs to the current game log file so
    // the Game Logs tab can show them even after the fact.
    if source == "launch" {
        if let Some(path) = crate::game_logs::get_current_log_path() {
            crate::game_logs::append_game_log_line(
                &path,
                &format!("[{}] [{}] {}", level.to_uppercase(), source, message),
            );
        }
    }
}

/// Emit a "launch" log message to the frontend ONLY — without appending to
/// the current game session file. Used for messages that are already written
/// to the session file by another writer in guaranteed chronological order
/// (e.g. the final "Game exited" line comes from the pipe-reader thread).
pub fn emit_launch_event(app: &AppHandle, level: &str, message: &str) {
    match level {
        "error" => tracing::error!(target: "launcher", source = "launch", "{}", message),
        "warn"  => tracing::warn!(target: "launcher", source = "launch", "{}", message),
        _       => tracing::info!(target: "launcher", source = "launch", "{}", message),
    }
    let _ = app.emit(
        "log_message",
        LogPayload {
            level: level.to_string(),
            source: "launch".to_string(),
            message: message.to_string(),
        },
    );
}

/// Spawn a background task that bridges broadcast channel → Tauri events
pub fn spawn_event_bridge(
    app: AppHandle,
    mut rx: broadcast::Receiver<InstallProgressPayload>,
    instance_id: String,
) {
    tokio::spawn(async move {
        while let Ok(payload) = rx.recv().await {
            let _ = app.emit("install_progress", &payload);
        }
        // Signal completion
        let _ = app.emit(
            "install_progress",
            &InstallProgressPayload {
                instance_id,
                percent: 100.0,
                downloaded_bytes: 0,
                total_bytes: 0,
                stage: "done".into(),
                message: "Installation complete".into(),
            },
        );
    });
}

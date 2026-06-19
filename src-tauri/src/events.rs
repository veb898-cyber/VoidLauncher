use serde::Serialize;
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

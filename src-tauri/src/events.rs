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

/// Emit a log message event to the frontend
pub fn emit_log(app: &AppHandle, level: &str, source: &str, message: &str) {
    let _ = app.emit(
        "log_message",
        LogPayload {
            level: level.to_string(),
            source: source.to_string(),
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

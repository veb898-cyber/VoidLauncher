use std::path::Path;
use std::sync::Once;
use tracing_subscriber::{fmt, EnvFilter};

/// Path to the active log file: `<data_dir>/logs/launcher.log`.
/// Read by the frontend's Settings "Open log" button (if added) and by
/// anyone debugging "why is the launcher broken" after a crash.
pub fn log_path(data_dir: &Path) -> std::path::PathBuf {
    data_dir.join("logs").join("launcher.log")
}

/// Initialize the file-based logger. Writes every `tracing::*!` event
/// (and every `events::emit_log` call — see `events.rs`) to
/// `<data_dir>/logs/launcher.log` in append mode.
///
/// Idempotent: safe to call from tests or multiple times — only the
/// first call wins. Falls back to `eprintln!` if the log file cannot
/// be opened (e.g. read-only filesystem, missing permissions), so the
/// app still starts even when logging is broken.
///
/// The non-blocking writer's `WorkerGuard` is intentionally leaked
/// into a `Box` so it lives until process exit. The OS reclaims it
/// at termination; for a desktop app that exits when the user closes
/// the window, this is the correct tradeoff (no global to thread
/// through, no risk of dropping the guard and losing buffered logs).
pub fn init(data_dir: &Path) {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let logs_dir = data_dir.join("logs");
        if let Err(e) = std::fs::create_dir_all(&logs_dir) {
            eprintln!(
                "[logger] Failed to create logs directory {:?}: {}. \
                 Continuing without file logging.",
                logs_dir, e
            );
            return;
        }

        let log_path = log_path(data_dir);
        let file = match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
        {
            Ok(f) => f,
            Err(e) => {
                eprintln!(
                    "[logger] Failed to open log file {:?}: {}. \
                     Continuing without file logging.",
                    log_path, e
                );
                return;
            }
        };

        // Background thread drains the in-memory buffer to the file.
        // We `Box::leak` the guard so the writer stays alive for the
        // entire process lifetime.
        let (non_blocking, guard) = tracing_appender::non_blocking(file);
        Box::leak(Box::new(guard));

        // Default filter: info for everything, debug for our own code.
        // Override at runtime by setting the `RUST_LOG` env var BEFORE
        // launching the .exe:
        //   $env:RUST_LOG = "debug"
        //   .\voidlauncher.exe
        let filter = EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new("info,voidlauncher_lib=debug"));

        let subscriber = fmt::Subscriber::builder()
            .with_env_filter(filter)
            .with_writer(non_blocking)
            .with_ansi(false)        // log file should be plain text
            .with_target(true)       // include module path
            .with_thread_names(false)
            .with_level(true)
            .with_file(false)
            .with_line_number(false)
            .finish();

        if tracing::subscriber::set_global_default(subscriber).is_ok() {
            // First-ever log lines. Use a separate channel (eprintln)
            // so they're visible even if tracing's set_global_default
            // fails on a second init in a test harness.
            eprintln!("[logger] File logging initialized: {}", log_path.display());
            tracing::info!(target: "launcher", "=== VoidLauncher starting ===");
            tracing::info!(target: "launcher", "Log file: {}", log_path.display());
        } else {
            eprintln!("[logger] A tracing subscriber was already set; skipping file logger init.");
        }
    });
}

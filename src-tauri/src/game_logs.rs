use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// Metadata for a game log session
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GameLogSession {
    pub path: String,
    pub instance_name: String,
    pub started_at: String,
    pub size_bytes: u64,
}

/// Tracks the current game session log path
static CURRENT_LOG_PATH: Mutex<Option<String>> = Mutex::new(None);

/// Sanitize an instance name for use inside a session filename.
/// Shared by `create_game_log_file` and by `cmd_list_game_logs` so the
/// frontend can filter sessions by instance without guessing the scheme.
pub fn sanitize_instance_name(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

// ---------------------------------------------------------------------------
// Live process output capture (single chronological stream)
//
// Prism-style logging: BOTH pipes of the game process are drained
// concurrently and every line is appended — as it arrives — into the SAME
// session file that already receives the launcher's own "launch" messages.
// The result is one interleaved stream (launcher setup -> JVM boot ->
// modloading -> gameplay), instead of two disconnected halves.
//
// Raw bytes are additionally teed into .stdout.log/.stderr.log for crash
// forensics (these are NOT listed as sessions).
//
// Exit ordering: the wait-task stores the exit code here via
// `mark_game_exit`; whichever reader thread finishes LAST writes the final
// "Game exited with code N" line, guaranteeing no line can land after it.
// ---------------------------------------------------------------------------

struct OutputReaderShared {
    session_path: Option<String>,
    remaining: AtomicUsize,
    exit_code: AtomicI32,
    exit_set: Arc<AtomicBool>,
}

static EXIT_REGISTRY: Mutex<Option<HashMap<u32, Arc<OutputReaderShared>>>> = Mutex::new(None);

fn with_exit_registry<R>(
    f: impl FnOnce(&mut HashMap<u32, Arc<OutputReaderShared>>) -> R,
) -> Option<R> {
    let mut guard = EXIT_REGISTRY.lock().ok()?;
    let reg = guard.get_or_insert_with(HashMap::new);
    Some(f(reg))
}

const EXIT_WAIT_MAX_MS: u64 = 3000;

fn pump_stream(
    stream: Box<dyn std::io::Read + Send>,
    mut raw: Option<std::io::BufWriter<std::fs::File>>,
    shared: Arc<OutputReaderShared>,
) {
    use std::io::{BufRead, BufReader, Write};
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        // Tee to the forensic raw file if available (flush per line so
        // crashes keep data).
        if let Some(w) = raw.as_mut() {
            if writeln!(w, "{}", line).is_err() {
                break;
            }
        }
        // Append to the unified session log in arrival order.
        if let Some(path) = &shared.session_path {
            append_game_log_line(path, &line);
        }
    }
    finish_stream(&shared);
}

/// Called by each reader on EOF; the LAST one finalizes the run.
fn finish_stream(shared: &Arc<OutputReaderShared>) {
    if shared.remaining.fetch_sub(1, Ordering::SeqCst) != 1 {
        return;
    }
    // Wait (bounded) for the wait-task to record the exit code.
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(EXIT_WAIT_MAX_MS);
    while !shared.exit_set.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    if let Some(path) = &shared.session_path {
        let code = shared.exit_code.load(Ordering::SeqCst);
        append_game_log_line(path, &format!("[INFO] [launch] Game exited with code {}", code));
    }
}

/// Spawn reader threads for a freshly spawned game process. Called once per
/// launch right after `Command::spawn` with piped stdio.
pub fn attach_output_readers(
    pid: u32,
    stdout_pipe: Box<dyn std::io::Read + Send>,
    stderr_pipe: Box<dyn std::io::Read + Send>,
    raw_stdout_file: Option<std::fs::File>,
    raw_stderr_file: Option<std::fs::File>,
) {
    let shared = Arc::new(OutputReaderShared {
        session_path: get_current_log_path(),
        remaining: AtomicUsize::new(2),
        exit_code: AtomicI32::new(0),
        exit_set: Arc::new(AtomicBool::new(false)),
    });
    if with_exit_registry(|reg| reg.insert(pid, shared.clone())).is_none() {
        tracing::warn!(target: "launcher", "Exit registry unavailable for pid {}", pid);
    }

    let sh_out = shared.clone();
    std::thread::Builder::new()
        .name("game-stdout".into())
        .spawn(move || {
            pump_stream(stdout_pipe, raw_stdout_file.map(std::io::BufWriter::new), sh_out);
        })
        .ok();

    let sh_err = shared;
    std::thread::Builder::new()
        .name("game-stderr".into())
        .spawn(move || {
            pump_stream(stderr_pipe, raw_stderr_file.map(std::io::BufWriter::new), sh_err);
        })
        .ok();
}

/// Record the process exit code; the last finishing reader thread uses it
/// to append the final chronological line to the session log.
pub fn mark_game_exit(pid: u32, code: i32) {
    let shared = with_exit_registry(|reg| reg.get(&pid).cloned()).flatten();
    if let Some(shared) = shared {
        shared.exit_code.store(code, Ordering::SeqCst);
        shared.exit_set.store(true, Ordering::SeqCst);
    }
}

/// Set the current game session log path
pub fn set_current_log_path(path: String) {
    if let Ok(mut guard) = CURRENT_LOG_PATH.lock() {
        *guard = Some(path);
    }
}

/// Get the current game session log path
pub fn get_current_log_path() -> Option<String> {
    if let Ok(guard) = CURRENT_LOG_PATH.lock() {
        guard.clone()
    } else {
        None
    }
}

/// Clear the current game session log path
#[allow(dead_code)]
pub fn clear_current_log_path() {
    if let Ok(mut guard) = CURRENT_LOG_PATH.lock() {
        *guard = None;
    }
}

/// Create a new game log file for this session and return its path
pub fn create_game_log_file(data_dir: &PathBuf, instance_name: &str) -> Result<String, String> {
    let game_logs_dir = data_dir.join("logs").join("game");
    std::fs::create_dir_all(&game_logs_dir).map_err(|e| e.to_string())?;

    let now = chrono::Local::now();
    let timestamp = now.format("%Y%m%d_%H%M%S");
    let safe_name = sanitize_instance_name(instance_name);
    let filename = format!("{}_{}.log", safe_name, timestamp);
    let path = game_logs_dir.join(&filename);

    // Write header with metadata
    let header = format!(
        "VoidLauncher Game Log\nInstance: {}\nStarted: {}\n{}\n",
        instance_name,
        now.format("%Y-%m-%d %H:%M:%S"),
        "=".repeat(60),
    );
    std::fs::write(&path, &header).map_err(|e| e.to_string())?;

    let path_str = path.to_string_lossy().to_string();
    set_current_log_path(path_str.clone());

    Ok(path_str)
}

/// Append a line to the given game log file
pub fn append_game_log_line(log_path: &str, line: &str) {
    use std::io::Write;
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(log_path)
    {
        let _ = writeln!(file, "{}", line);
    }
}

/// List game log sessions (most recent first, max 7)
pub fn list_game_log_sessions(data_dir: &PathBuf) -> Vec<GameLogSession> {
    let game_logs_dir = data_dir.join("logs").join("game");
    if !game_logs_dir.exists() {
        return Vec::new();
    }

    let mut sessions: Vec<GameLogSession> = Vec::new();

    if let Ok(entries) = std::fs::read_dir(&game_logs_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("log") {
                continue;
            }
            // Skip the raw stdout/stderr tee files — they are forensic
            // copies, not sessions (stem looks like "X_..._123456.stdout").
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                if stem.ends_with(".stdout") || stem.ends_with(".stderr") {
                    continue;
                }
            }
            if let Ok(meta) = path.metadata() {
                if meta.is_file() {
                    let file_name = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("unknown")
                        .to_string();

                    // Parse instance name from filename.
                    // Format: {safe_name}_{YYYYMMDD}_{HHMMSS} — strip the
                    // two trailing timestamp segments to recover the name.
                    let instance_name = file_name
                        .rsplitn(3, '_')
                        .last()
                        .unwrap_or(&file_name)
                        .to_string();

                    let started_at = if let Ok(modified) = meta.modified() {
                        let dt: chrono::DateTime<chrono::Local> = modified.into();
                        dt.format("%Y-%m-%d %H:%M:%S").to_string()
                    } else {
                        String::new()
                    };

                    sessions.push(GameLogSession {
                        path: path.to_string_lossy().to_string(),
                        instance_name,
                        started_at,
                        size_bytes: meta.len(),
                    });
                }
            }
        }
    }

    // Sort by modification time, most recent first
    sessions.sort_by(|a, b| {
        let a_path = PathBuf::from(&a.path);
        let b_path = PathBuf::from(&b.path);
        let a_modified = a_path.metadata().ok().and_then(|m| m.modified().ok());
        let b_modified = b_path.metadata().ok().and_then(|m| m.modified().ok());
        b_modified.cmp(&a_modified)
    });

    sessions
}

/// Read a game log file and return its content (with line limit)
pub fn read_game_log(path: &str, max_lines: Option<usize>) -> Result<String, String> {
    let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let max = max_lines.unwrap_or(5000);
    let lines: Vec<&str> = content.lines().collect();
    if lines.len() > max {
        let truncated = lines[lines.len().saturating_sub(max)..].join("\n");
        Ok(format!("... (showing last {} lines)\n{}", max, truncated))
    } else {
        Ok(content)
    }
}

pub fn delete_game_log(data_dir: &PathBuf, path: &str) -> Result<(), String> {
    let safe_path = validate_log_path(data_dir, path)?;
    if safe_path == get_current_log_path().unwrap_or_default() {
        return Err("Cannot delete the currently active game log".to_string());
    }
    std::fs::remove_file(&safe_path).map_err(|e| format!("Failed to delete log: {}", e))
}

pub fn validate_log_path(data_dir: &PathBuf, path: &str) -> Result<String, String> {
    let logs_dir = data_dir.join("logs").join("game");
    let logs_canon = logs_dir
        .canonicalize()
        .map_err(|e| format!("Invalid logs directory: {}", e))?;
    let file_path = std::path::Path::new(path);
    let file_canon = file_path
        .canonicalize()
        .map_err(|e| format!("Invalid log file path: {}", e))?;
    if !file_canon.starts_with(&logs_canon) {
        return Err("Access denied: log file is outside the game logs directory".to_string());
    }
    Ok(file_canon.to_string_lossy().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Logs are NEVER rotated/deleted by the launcher (owner's decision:
    /// full on-disk history). The session list must therefore return every
    /// run, newest first, and raw stdout/stderr tee files must stay out of it.
    #[test]
    fn sessions_are_kept_forever_and_listed_newest_first() {
        let dir = std::env::temp_dir().join(format!("vl_gamelogs_test_{}", std::process::id()));
        let logs_dir = dir.join("logs").join("game");
        std::fs::create_dir_all(&logs_dir).unwrap();

        // Nine past launches of the SAME instance (names carry the date)
        // plus one raw tee file like the unified logger produces.
        for i in 1..=9u32 {
            let name = format!("Inst_20260820_0000{:02}.log", i);
            std::fs::write(logs_dir.join(&name), format!("run {}\n", i)).unwrap();
            // Ensure strictly increasing mtimes so the mtime sort is stable.
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        std::fs::write(
            logs_dir.join("Inst_20260820_000005.stdout.log"),
            "raw\n",
        )
        .unwrap();

        // A "new launch" only creates a new file — nothing is removed.
        create_game_log_file(&dir, "Inst").unwrap();

        let sessions = list_game_log_sessions(&dir);
        assert!(sessions.iter().all(|s| !s.path.ends_with(".stdout.log")));
        assert_eq!(sessions.len(), 10, "all runs kept, rotation disabled");
        for i in 1..=9u32 {
            assert!(logs_dir.join(format!("Inst_20260820_0000{:02}.log", i)).exists());
        }
        // Newest-first ordering for the UI (the fresh run is last created).
        let newest = std::path::Path::new(&sessions[0].path)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(newest.starts_with("Inst_2"), "fresh run listed first: {}", newest);

        // Instance-name sanitization shared by creation and filtering.
        assert_eq!(sanitize_instance_name("Better MC [FORGE] BMC4"), "Better_MC__FORGE__BMC4");

        clear_current_log_path();
        std::fs::remove_dir_all(&dir).ok();
    }
}

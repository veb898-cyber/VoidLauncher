use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;

const MAX_LOG_FILES: usize = 5;

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
    let safe_name =
        instance_name.replace(|c: char| !c.is_alphanumeric() && c != '-' && c != '_', "_");
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

    // Rotate old logs
    rotate_game_logs(data_dir);

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

/// List game log sessions (most recent first, max 5)
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

/// Rotate old game logs to keep only MAX_LOG_FILES most recent
fn rotate_game_logs(data_dir: &PathBuf) {
    let sessions = list_game_log_sessions(data_dir);
    if sessions.len() <= MAX_LOG_FILES {
        return;
    }
    for session in sessions.iter().skip(MAX_LOG_FILES) {
        if session.path != get_current_log_path().unwrap_or_default() {
            let _ = std::fs::remove_file(&session.path);
        }
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

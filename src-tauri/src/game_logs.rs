use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

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

/// Hard cap for ONE incremental update sent to the renderer. A crash-looping
/// game can append hundreds of megabytes between two polls; without a cap the
/// read allocates that whole delta (and serializes it over IPC). When the
/// delta exceeds the cap only the newest complete lines are returned and the
/// offset still advances, so later polls remain correct.
pub const MAX_INCREMENTAL_LOG_BYTES: u64 = 4 * 1024 * 1024;

/// Hard cap for the initial tail snapshot / "show last N lines" read. 100k
/// lines of a pathological log can otherwise be arbitrarily large.
const MAX_TAIL_LOG_BYTES: u64 = 16 * 1024 * 1024;

/// Cache of the `Instance:` header per session path. Session filenames are
/// unique and the header never changes, so listing hundreds of sessions every
/// second must not re-open and re-read every file.
static LOG_HEADER_CACHE: Mutex<Option<HashMap<std::path::PathBuf, String>>> = Mutex::new(None);

fn cached_instance_header(path: &std::path::Path) -> Option<String> {
    {
        let guard = LOG_HEADER_CACHE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(cache) = guard.as_ref() {
            if let Some(name) = cache.get(path) {
                return Some(name.clone());
            }
        }
    }
    let name = read_instance_header(path)?;
    let mut guard = LOG_HEADER_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let cache = guard.get_or_insert_with(HashMap::new);
    // Keep the cache bounded when users accumulate years of sessions.
    if cache.len() > 2000 {
        cache.clear();
    }
    cache.insert(path.to_path_buf(), name.clone());
    Some(name)
}

/// Sanitize an instance name for use inside a session filename.
/// Shared by `create_game_log_file` and by `cmd_list_game_logs` so the
/// frontend can filter sessions by instance without guessing the scheme.
pub fn sanitize_instance_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
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
    pid: u32,
    session_path: Option<String>,
    remaining: AtomicUsize,
    exit_code: AtomicI32,
    exit_set: Arc<AtomicBool>,
    /// Set when the LAST reader reached EOF and flushed its raw tee file.
    /// `readers_done_cv` wakes callers of [`wait_for_output_readers`].
    readers_done: Mutex<bool>,
    readers_done_cv: Condvar,
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
    // Flush the forensic copy explicitly and drop it BEFORE reporting the
    // reader as finished: callers (and the smoke harness) read these files
    // immediately after `wait_for_output_readers`.
    if let Some(w) = raw.as_mut() {
        let _ = w.flush();
    }
    drop(raw);
    finish_stream(&shared);
}

/// Called by each reader on EOF; the LAST one finalizes the run.
fn finish_stream(shared: &Arc<OutputReaderShared>) {
    if shared.remaining.fetch_sub(1, Ordering::SeqCst) != 1 {
        return;
    }
    // Both readers have reached EOF and flushed their raw files: wake waiters
    // now, before the bounded exit-code bookkeeping below (which may take up
    // to `EXIT_WAIT_MAX_MS` when no exit watcher is running, e.g. in the
    // smoke harness).
    if let Ok(mut done) = shared.readers_done.lock() {
        *done = true;
        shared.readers_done_cv.notify_all();
    }
    // Wait (bounded) for the wait-task to record the exit code.
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(EXIT_WAIT_MAX_MS);
    while !shared.exit_set.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    if let Some(path) = &shared.session_path {
        let code = shared.exit_code.load(Ordering::SeqCst);
        append_game_log_line(
            path,
            &format!("[INFO] [launch] Game exited with code {}", code),
        );
    }
    // The run is over — drop its registry entry so long sessions don't leak
    // one Arc<OutputReaderShared> per launch for the launcher's lifetime.
    with_exit_registry(|reg| reg.remove(&shared.pid));
}

/// Spawn reader threads for a freshly spawned game process. Called once per
/// launch right after `Command::spawn` with piped stdio. `session_path` is
/// the session file this run must append to — passed explicitly so that
/// launching instance B while instance A is still running cannot redirect
/// A's output into B's log (and vice versa).
pub fn attach_output_readers(
    pid: u32,
    stdout_pipe: Box<dyn std::io::Read + Send>,
    stderr_pipe: Box<dyn std::io::Read + Send>,
    raw_stdout_file: Option<std::fs::File>,
    raw_stderr_file: Option<std::fs::File>,
    session_path: Option<String>,
) {
    let shared = Arc::new(OutputReaderShared {
        pid,
        session_path,
        remaining: AtomicUsize::new(2),
        exit_code: AtomicI32::new(0),
        exit_set: Arc::new(AtomicBool::new(false)),
        readers_done: Mutex::new(false),
        readers_done_cv: Condvar::new(),
    });
    if with_exit_registry(|reg| reg.insert(pid, shared.clone())).is_none() {
        tracing::warn!(target: "launcher", "Exit registry unavailable for pid {}", pid);
    }

    let sh_out = shared.clone();
    std::thread::Builder::new()
        .name("game-stdout".into())
        .spawn(move || {
            pump_stream(
                stdout_pipe,
                raw_stdout_file.map(std::io::BufWriter::new),
                sh_out,
            );
        })
        .ok();

    let sh_err = shared;
    std::thread::Builder::new()
        .name("game-stderr".into())
        .spawn(move || {
            pump_stream(
                stderr_pipe,
                raw_stderr_file.map(std::io::BufWriter::new),
                sh_err,
            );
        })
        .ok();
}

/// Result of [`wait_for_output_readers`].
// `smoke_launch` (cfg(test)) is the caller; the API stays available outside
// tests on purpose.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadersWait {
    /// Both readers reached EOF and flushed their raw tee files — every byte
    /// the game wrote is on disk.
    Finished,
    /// No readers are registered for this pid (they were never attached, or
    /// they already finished and cleaned up).
    Unknown,
    /// The deadline passed while a reader was still draining.
    TimedOut,
}

/// Block (bounded by `timeout`) until both output readers of `pid` have
/// reached EOF and flushed their raw `.stdout.log`/`.stderr.log` files.
///
/// A broken reader can never hang the caller: the wait always ends, and the
/// caller can tell the difference between "finished" and "timed out".
#[allow(dead_code)]
pub fn wait_for_output_readers(pid: u32, timeout: Duration) -> ReadersWait {
    let shared = with_exit_registry(|reg| reg.get(&pid).cloned()).flatten();
    let Some(shared) = shared else {
        return ReadersWait::Unknown;
    };

    let Ok(done) = shared.readers_done.lock() else {
        return ReadersWait::TimedOut;
    };
    if *done {
        return ReadersWait::Finished;
    }
    let deadline = Instant::now() + timeout;
    let mut guard = done;
    loop {
        let now = Instant::now();
        if now >= deadline {
            return if *guard {
                ReadersWait::Finished
            } else {
                ReadersWait::TimedOut
            };
        }
        let (next, result) = shared
            .readers_done_cv
            .wait_timeout(guard, deadline - now)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard = next;
        if *guard {
            return ReadersWait::Finished;
        }
        if result.timed_out() {
            return ReadersWait::TimedOut;
        }
    }
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

/// Clear the current session pointer only if it still points at `path`.
/// The exit watcher calls this with its own run's path: with several games
/// running, a game that exits must not wipe the pointer that now belongs to
/// a newer launch.
pub fn clear_current_log_path_if(path: &str) {
    if let Ok(mut guard) = CURRENT_LOG_PATH.lock() {
        if guard.as_deref() == Some(path) {
            *guard = None;
        }
    }
}

/// Parse the launch timestamp out of a session filename
/// (`{safe_name}_{YYYYMMDD}_{HHMMSS}.log`) in the local zone. None when the
/// name doesn't follow the scheme (raw tee files, hand-copied logs).
pub fn session_started_at(path: &Path) -> Option<std::time::SystemTime> {
    use chrono::TimeZone;
    let stem = path.file_stem()?.to_str()?;
    let mut parts = stem.rsplitn(3, '_');
    let hms = parts.next()?;
    let ymd = parts.next()?;
    if ymd.len() != 8 || hms.len() != 6 {
        return None;
    }
    let combined = format!("{}{}", ymd, hms);
    let naive = chrono::NaiveDateTime::parse_from_str(&combined, "%Y%m%d%H%M%S").ok()?;
    let local = chrono::Local.from_local_datetime(&naive).earliest()?;
    Some(local.into())
}

/// Newest session log of a specific instance (by mtime). Used by the crash
/// analyzer when it has no explicit session path — the auto-analysis after
/// exit runs after `CURRENT_LOG_PATH` was already cleared, so the "current"
/// path is gone by then.
pub fn newest_session_for_instance(data_dir: &Path, instance_name: &str) -> Option<PathBuf> {
    let dir = data_dir.join("logs").join("game");
    let safe = sanitize_instance_name(instance_name);
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(&dir).ok()?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("log") {
            continue;
        }
        // Skip the raw stdout/stderr tee copies (stem ends with .stdout /
        // .stderr), mirroring the session-list filter.
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if stem.ends_with(".stdout") || stem.ends_with(".stderr") {
            continue;
        }
        // The name recovers the instance the same way the session list does:
        // rsplit off the two timestamp segments, compare the remainder.
        if stem.rsplitn(3, '_').last() != Some(safe.as_str()) {
            continue;
        }
        let mtime = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        if best.as_ref().map(|(t, _)| mtime > *t).unwrap_or(true) {
            best = Some((mtime, path));
        }
    }
    best.map(|(_, p)| p)
}

/// Create a new game log file for this session and return its path
pub fn create_game_log_file(data_dir: &Path, instance_name: &str) -> Result<String, String> {
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
        now.format("%d/%m/%Y %H:%M:%S"),
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

/// Read the real instance name from a game-log header line
/// ("Instance: <name>", see `create_game_log_file`). Returns None for files
/// without a recognizable header (e.g. hand-copied logs).
fn read_instance_header(path: &Path) -> Option<String> {
    use std::io::BufRead;
    let file = std::fs::File::open(path).ok()?;
    for line in std::io::BufReader::new(file).lines().take(10).flatten() {
        if let Some(rest) = line.strip_prefix("Instance: ") {
            let name = rest.trim();
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }
    None
}

/// List game log sessions (most recent first, max 7)
pub fn list_game_log_sessions(data_dir: &Path) -> Vec<GameLogSession> {
    let game_logs_dir = data_dir.join("logs").join("game");
    if !game_logs_dir.exists() {
        return Vec::new();
    }

    let mut sessions: Vec<(Option<std::time::SystemTime>, GameLogSession)> = Vec::new();

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

                    // Recover the REAL instance name from the log header
                    // ("Instance: <name>"). Fall back to the sanitized
                    // filename stem ({safe_name}_{YYYYMMDD}_{HHMMSS} — strip
                    // the two trailing timestamp segments): characters like
                    // spaces or brackets sanitize to '_' and cannot be
                    // reconstructed, yet crash analysis needs the exact name
                    // to find the instance directory.
                    let fallback_name = file_name
                        .rsplitn(3, '_')
                        .last()
                        .unwrap_or(&file_name)
                        .to_string();
                    let instance_name = cached_instance_header(&path).unwrap_or(fallback_name);

                    let modified = meta.modified().ok();
                    let started_at = if let Some(modified) = modified {
                        let dt: chrono::DateTime<chrono::Local> = modified.into();
                        dt.format("%d/%m/%Y %H:%M:%S").to_string()
                    } else {
                        String::new()
                    };

                    sessions.push((
                        modified,
                        GameLogSession {
                            path: path.to_string_lossy().to_string(),
                            instance_name,
                            started_at,
                            size_bytes: meta.len(),
                        },
                    ));
                }
            }
        }
    }

    // Sort by modification time, most recent first. The timestamp was already
    // read from metadata above: hundreds of sessions must not be re-stat'ed on
    // every poll.
    sessions.sort_by_key(|b| std::cmp::Reverse(b.0));
    sessions.into_iter().map(|(_, session)| session).collect()
}

/// Read a game log file and return its content (with line limit)
pub fn read_game_log(path: &str, max_lines: Option<usize>) -> Result<String, String> {
    let max = max_lines.unwrap_or(5000);
    // Read only the tail of the log instead of loading the whole file:
    // sessions can grow to hundreds of thousands of lines, and this command
    // is polled every second while the console is open. A full read would
    // re-materialize the entire file in RAM on every poll.
    let (content, truncated) = read_tail_lossy(path, max);
    if truncated {
        Ok(format!("... (showing last {} lines)\n{}", max, content))
    } else {
        Ok(content)
    }
}

/// Read the trailing `max` lines of a file, decoding only those bytes. Scans
/// backwards from the end for `max` newline boundaries and returns the bytes
/// after the last one, falling back to the whole file when it is smaller.
/// The returned bool is `true` when the file held more lines than `max` (i.e.
/// the result is a truncated tail rather than the complete file).
fn read_tail_lossy(path: &str, max: usize) -> (String, bool) {
    let Ok(file) = std::fs::File::open(path) else {
        return (String::new(), false);
    };
    let (bytes, truncated, _, _) = read_tail_bytes_from(&file, max);
    (String::from_utf8_lossy(&bytes).to_string(), truncated)
}

/// Read the trailing `max` lines through an already-open handle. Returns the
/// raw bytes, whether the result was truncated to `max` lines, the absolute
/// offset the returned tail starts at and the file length captured before
/// reading. The read is capped to that length, so a log being appended to
/// cannot return bytes beyond the offset the caller will store next.
fn read_tail_bytes_from(file: &std::fs::File, max: usize) -> (Vec<u8>, bool, u64, u64) {
    let file_len = match file.metadata() {
        Ok(m) => m.len(),
        Err(_) => return (Vec::new(), false, 0, 0),
    };
    if max == 0 {
        return (Vec::new(), true, file_len, file_len);
    }
    const CHUNK: u64 = 64 * 1024;

    use std::io::Read;
    use std::io::Seek;
    use std::io::SeekFrom;
    let mut reader = match file.try_clone() {
        Ok(f) => f,
        Err(_) => return (Vec::new(), false, 0, file_len),
    };

    // Collect, newest-first, the *absolute byte offsets* of newline characters,
    // walking backwards in 64 KiB blocks. We gather up to `max + 1` so that
    // removing a trailing (empty-line) newline below still leaves `max` usable
    // boundaries. The loop stops when it has enough or reaches file start
    // (returning everything).
    let mut boundaries: Vec<u64> = Vec::with_capacity(max + 1);
    let mut block_end: u64 = file_len;
    let mut buf = vec![0u8; CHUNK as usize];
    // Stop the backwards scan at the same byte budget used for the read:
    // a multi-gigabyte file with very long lines must not be walked end to
    // end just to discover it has fewer than `max` newlines.
    while block_end > 0
        && boundaries.len() < max + 1
        && file_len.saturating_sub(block_end) < MAX_TAIL_LOG_BYTES
    {
        let read_start = block_end.saturating_sub(CHUNK);
        let len = (block_end - read_start) as usize;
        if reader.seek(SeekFrom::Start(read_start)).is_err() {
            break;
        }
        if reader.read_exact(&mut buf[..len]).is_err() {
            break;
        }
        // Record newlines newest-first (walk this block back-to-front).
        let piece = &buf[..len];
        for i in (0..len).rev() {
            if boundaries.len() > max {
                break;
            }
            if piece[i] == b'\n' {
                boundaries.push(read_start + i as u64);
            }
        }
        block_end = read_start;
    }

    // If the file's final newline sits at its very end, the byte after it is
    // empty and doesn't start a visible line — drop it so the "last N lines"
    // wording stays truthful.
    if file_len > 0 && boundaries.first() == Some(&(file_len - 1)) {
        boundaries.remove(0);
    }

    let tail_start = if boundaries.len() >= max {
        // `max`-th newline from the end: first line starts right after it.
        boundaries[max - 1] + 1
    } else {
        // Fewer newlines than asked for — include the whole file.
        0
    };
    let mut truncated = tail_start > 0;
    let mut tail_start = tail_start;
    // Cap the byte window as well as the line count. A file with very long
    // lines can otherwise force a huge allocation from a "last 100k lines"
    // request. Skip the leading partial line so the caller never receives a
    // spliced line.
    let mut drop_leading_partial = false;
    if file_len.saturating_sub(tail_start) > MAX_TAIL_LOG_BYTES {
        tail_start = file_len - MAX_TAIL_LOG_BYTES;
        truncated = true;
        drop_leading_partial = true;
    }

    let mut tail = Vec::new();
    if reader.seek(SeekFrom::Start(tail_start)).is_err() {
        return (Vec::new(), truncated, tail_start, file_len);
    }
    // Never read beyond the length captured above: bytes appended while this
    // function runs must be returned by the next poll, not twice.
    let remaining = file_len.saturating_sub(tail_start);
    let mut limited = reader.take(remaining);
    if limited.read_to_end(&mut tail).is_err() {
        return (Vec::new(), truncated, tail_start, file_len);
    }
    if drop_leading_partial {
        match tail.iter().position(|&b| b == b'\n') {
            Some(pos) => {
                tail.drain(..=pos);
                tail_start += pos as u64 + 1;
            }
            None => {
                // One giant unterminated line fills the whole window.
                return (Vec::new(), true, file_len, file_len);
            }
        }
    }
    (tail, truncated, tail_start, file_len)
}

pub fn delete_game_log(data_dir: &Path, path: &str) -> Result<(), String> {
    let safe_path = validate_log_path(data_dir, path)?;
    if safe_path == get_current_log_path().unwrap_or_default() {
        return Err("Cannot delete the currently active game log".to_string());
    }
    std::fs::remove_file(&safe_path).map_err(|e| format!("Failed to delete log: {}", e))
}

pub fn validate_log_path(data_dir: &Path, path: &str) -> Result<String, String> {
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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct IncrementalLogChunk {
    pub lines: Vec<String>,
    pub new_byte_offset: u64,
    pub reset: bool,
    pub file_token: String,
}

#[cfg(windows)]
fn get_file_identity(file: &std::fs::File) -> (u64, u64) {
    use std::os::windows::io::AsRawHandle;
    #[repr(C)]
    struct BY_HANDLE_FILE_INFORMATION {
        dw_file_attributes: u32,
        ft_creation_time: [u32; 2],
        ft_last_access_time: [u32; 2],
        ft_last_write_time: [u32; 2],
        dw_volume_serial_number: u32,
        n_file_size_high: u32,
        n_file_size_low: u32,
        n_number_of_links: u32,
        n_file_index_high: u32,
        n_file_index_low: u32,
    }
    extern "system" {
        fn GetFileInformationByHandle(
            handle: *mut std::ffi::c_void,
            info: *mut BY_HANDLE_FILE_INFORMATION,
        ) -> i32;
    }

    let mut info = std::mem::MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::zeroed();
    let handle = file.as_raw_handle();
    let res = unsafe { GetFileInformationByHandle(handle, info.as_mut_ptr()) };
    if res != 0 {
        let info = unsafe { info.assume_init() };
        let file_index = ((info.n_file_index_high as u64) << 32) | (info.n_file_index_low as u64);
        let vol_and_time = ((info.dw_volume_serial_number as u64) << 32)
            ^ (((info.ft_creation_time[1] as u64) << 32) | (info.ft_creation_time[0] as u64));
        (file_index, vol_and_time)
    } else {
        use std::os::windows::fs::MetadataExt;
        if let Ok(meta) = file.metadata() {
            (meta.creation_time(), meta.file_attributes() as u64)
        } else {
            (0, 0)
        }
    }
}

#[cfg(unix)]
fn get_file_identity(file: &std::fs::File) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    if let Ok(meta) = file.metadata() {
        (meta.ino(), meta.dev())
    } else {
        (0, 0)
    }
}

#[cfg(not(any(windows, unix)))]
fn get_file_identity(_file: &std::fs::File) -> (u64, u64) {
    (0, 0)
}

/// Build a token that detects log rotation, replacement and in-place rewrites
/// across incremental polls.
///
/// Guarantees:
/// - File identity (volume serial + file index on Windows, inode/dev on Unix)
///   is always hashed, so replacing the file is detected even when the new
///   file has the same size and prefix.
/// - Truncation is also detected by the caller (`file_len < byte_offset`).
/// - For acknowledged offsets up to 256 KiB the whole `0..byte_offset` range is
///   hashed: any in-place change there is detected deterministically.
/// - For larger offsets only a bounded sample is hashed: the first 4 KiB, the
///   last 4 KiB before the offset and 16 checkpoints of 256 bytes. A small,
///   isolated in-place rewrite in an unsampled middle region can therefore go
///   undetected until the offset advances over it. Append-only writes (the
///   normal log path) never invalidate the token because acknowledged bytes do
///   not change.
///
/// If `GetFileInformationByHandle` fails, identity falls back to creation time
/// and file attributes, which is weaker than the volume/file-index pair; the
/// content sample is still included.
pub fn compute_file_token(file: &std::fs::File, byte_offset: u64) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let (id1, id2) = get_file_identity(file);

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    use std::hash::Hasher;
    hasher.write_u64(id1);
    hasher.write_u64(id2);

    let mut f = match file.try_clone() {
        Ok(cloned) => cloned,
        Err(_) => return format!("{:016x}", hasher.finish()),
    };

    // 1. Always sample first line (up to 512 bytes)
    let mut header = [0u8; 512];
    let _ = f.seek(SeekFrom::Start(0));
    let n = f.read(&mut header).unwrap_or(0);
    let first_line = if let Some(pos) = header[..n].iter().position(|&b| b == b'\n') {
        &header[..=pos]
    } else {
        &header[..n]
    };
    hasher.write(first_line);

    // 2. Bind to acknowledged bytes in 0..byte_offset.
    // In an append-only file, past bytes never change on subsequent polls.
    // In an in-place rewrite, past bytes differ and trigger a reset.
    if byte_offset > 0 {
        const MAX_FULL_HASH_BYTES: u64 = 256 * 1024; // 256 KB
        if byte_offset <= MAX_FULL_HASH_BYTES {
            // For files/offsets up to 256 KB, read and hash the entire acknowledged range 0..byte_offset.
            // This provides 100% deterministic detection of any modification in any part of the file.
            let _ = f.seek(SeekFrom::Start(0));
            let mut buf = vec![0u8; byte_offset as usize];
            if f.read_exact(&mut buf).is_ok() {
                hasher.write(&buf);
            } else if let Ok(read_bytes) = f.read(&mut buf) {
                hasher.write(&buf[..read_bytes]);
            }
        } else {
            // For large files (> 256 KB), full re-reads every second would degrade I/O.
            // We use multi-point checkpoint sampling:
            // - Head window: first 4 KB
            // - Tail window: last 4 KB before byte_offset
            // - 16 evenly distributed checkpoints across the middle span
            let mut chunk = [0u8; 4096];
            let _ = f.seek(SeekFrom::Start(0));
            if let Ok(rn) = f.read(&mut chunk) {
                hasher.write(&chunk[..rn]);
            }

            let tail_start = byte_offset.saturating_sub(4096);
            if f.seek(SeekFrom::Start(tail_start)).is_ok() {
                if let Ok(rn) = f.read(&mut chunk) {
                    hasher.write(&chunk[..rn]);
                }
            }

            let middle_start = 4096u64;
            let middle_end = tail_start;
            if middle_end > middle_start {
                let span = middle_end - middle_start;
                let step = span / 17;
                let mut cp_buf = [0u8; 256];
                for i in 1..=16 {
                    let pos = middle_start + step * i;
                    if f.seek(SeekFrom::Start(pos)).is_ok() {
                        if let Ok(rn) = f.read(&mut cp_buf) {
                            hasher.write(&cp_buf[..rn]);
                        }
                    }
                }
            }
        }
    }

    format!("{:016x}", hasher.finish())
}

pub fn read_game_log_incremental(
    path: &str,
    byte_offset: u64,
    initial_max_lines: Option<usize>,
    file_token: Option<&str>,
) -> Result<IncrementalLogChunk, String> {
    use std::io::{Read, Seek, SeekFrom};

    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) => return Err(format!("Cannot open log file: {}", e)),
    };
    let meta = file.metadata().map_err(|e| e.to_string())?;
    let file_len = meta.len();
    let current_token = compute_file_token(&file, byte_offset);
    let token_mismatch = match file_token {
        Some(expected) => expected != current_token,
        None => false,
    };

    // If offset is 0, or file has been truncated / rotated (file_len < byte_offset),
    // or file identity has changed (token mismatch)
    if byte_offset == 0 || file_len < byte_offset || token_mismatch {
        // The UI asks for 100k lines; never let a bad caller request an
        // unbounded tail allocation.
        let max = initial_max_lines.unwrap_or(100_000).min(100_000);
        // Read through the same handle whose identity/token was computed. Only
        // complete lines are returned, and the next offset points right after
        // the last newline: a line that is still being written is delivered by
        // the next poll instead of being split in two.
        let (mut tail_bytes, _, tail_start, _) = read_tail_bytes_from(&file, max);
        let complete_len = tail_bytes
            .iter()
            .rposition(|&b| b == b'\n')
            .map(|pos| pos + 1)
            .unwrap_or(0);
        tail_bytes.truncate(complete_len);
        let lines: Vec<String> = String::from_utf8_lossy(&tail_bytes)
            .lines()
            .map(|l| l.to_string())
            .collect();
        let new_byte_offset = tail_start + complete_len as u64;
        let token_for_new_offset = compute_file_token(&file, new_byte_offset);
        return Ok(IncrementalLogChunk {
            lines,
            new_byte_offset,
            reset: true,
            file_token: token_for_new_offset,
        });
    }

    if file_len == byte_offset {
        return Ok(IncrementalLogChunk {
            lines: Vec::new(),
            new_byte_offset: file_len,
            reset: false,
            file_token: current_token,
        });
    }

    let mut reader = file;
    let delta = file_len - byte_offset;
    // A huge unread delta is delivered as its newest complete lines only.
    // The middle is dropped deliberately (the log file itself is untouched)
    // and the offset advances past it, so the next poll stays cheap and
    // correct instead of replaying hundreds of megabytes over IPC.
    let read_start = if delta > MAX_INCREMENTAL_LOG_BYTES {
        file_len - MAX_INCREMENTAL_LOG_BYTES
    } else {
        byte_offset
    };
    let capped = read_start > byte_offset;
    reader
        .seek(SeekFrom::Start(read_start))
        .map_err(|e| e.to_string())?;
    let bytes_to_read = (file_len - read_start) as usize;
    let mut buffer = vec![0u8; bytes_to_read];
    reader.read_exact(&mut buffer).map_err(|e| e.to_string())?;

    // The cap starts mid-line: drop that partial first line.
    let mut first_returned_byte = read_start;
    if capped {
        match buffer.iter().position(|&b| b == b'\n') {
            Some(pos) => {
                buffer.drain(..=pos);
                first_returned_byte += pos as u64 + 1;
            }
            None => {
                // The whole capped window is one unterminated line.
                let token = compute_file_token(&reader, file_len);
                return Ok(IncrementalLogChunk {
                    lines: Vec::new(),
                    new_byte_offset: file_len,
                    reset: false,
                    file_token: token,
                });
            }
        }
        tracing::warn!(
            target: "launcher",
            "Game log grew by {} bytes between polls; delivered only the newest {} bytes",
            delta,
            MAX_INCREMENTAL_LOG_BYTES
        );
    }

    // Find the last newline in buffer.
    // If a line is partially written (no newline yet) or a multi-byte UTF-8 character
    // is split across the read boundary, we ONLY advance up to the last complete newline!
    if let Some(last_nl) = buffer.iter().rposition(|&b| b == b'\n') {
        let valid_bytes = &buffer[..=last_nl];
        let text = String::from_utf8_lossy(valid_bytes);
        let lines: Vec<String> = text.lines().map(|l| l.to_string()).collect();
        let new_byte_offset = first_returned_byte + (last_nl as u64) + 1;
        let token_for_new_offset = compute_file_token(&reader, new_byte_offset);

        Ok(IncrementalLogChunk {
            lines,
            new_byte_offset,
            reset: false,
            file_token: token_for_new_offset,
        })
    } else if bytes_to_read > 64 * 1024 {
        // Line exceeds 64KB without newline: yield to avoid unbounded buffer stalling
        let text = String::from_utf8_lossy(&buffer);
        let lines: Vec<String> = text.lines().map(|l| l.to_string()).collect();
        let token_for_new_offset = compute_file_token(&reader, file_len);

        Ok(IncrementalLogChunk {
            lines,
            new_byte_offset: file_len,
            reset: false,
            file_token: token_for_new_offset,
        })
    } else {
        // Line is not completed yet; wait for the rest of the line and newline to be written
        Ok(IncrementalLogChunk {
            lines: Vec::new(),
            new_byte_offset: byte_offset,
            reset: false,
            file_token: current_token,
        })
    }
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
        std::fs::write(logs_dir.join("Inst_20260820_000005.stdout.log"), "raw\n").unwrap();

        // A "new launch" only creates a new file — nothing is removed.
        create_game_log_file(&dir, "Inst").unwrap();

        let sessions = list_game_log_sessions(&dir);
        assert!(sessions.iter().all(|s| !s.path.ends_with(".stdout.log")));
        assert_eq!(sessions.len(), 10, "all runs kept, rotation disabled");
        for i in 1..=9u32 {
            assert!(logs_dir
                .join(format!("Inst_20260820_0000{:02}.log", i))
                .exists());
        }
        // Newest-first ordering for the UI (the fresh run is last created).
        let newest = std::path::Path::new(&sessions[0].path)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(
            newest.starts_with("Inst_2"),
            "fresh run listed first: {}",
            newest
        );

        // Instance-name sanitization shared by creation and filtering.
        assert_eq!(
            sanitize_instance_name("Better MC [FORGE] BMC4"),
            "Better_MC__FORGE__BMC4"
        );

        clear_current_log_path();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The session filename carries the launch timestamp the crash
    /// analyzer needs to bound its report window to one run.
    #[test]
    fn session_filename_parses_start_time_and_finds_newest_run() {
        let dir = std::env::temp_dir().join(format!("vl_gamelogs_started_{}", std::process::id()));
        let logs_dir = dir.join("logs").join("game");
        std::fs::create_dir_all(&logs_dir).unwrap();

        let old = logs_dir.join("Inst_20260101_120000.log");
        let new = logs_dir.join("Inst_20260102_090000.log");
        std::fs::write(&old, "a\n").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&new, "b\n").unwrap();
        // Foreign names must not parse or be picked.
        std::fs::write(logs_dir.join("Inst_20260102_090000.stdout.log"), "raw\n").unwrap();
        std::fs::write(logs_dir.join("Other_20260103_100000.log"), "c\n").unwrap();

        let start = session_started_at(&old).expect("filename should parse");
        let start_dt: chrono::DateTime<chrono::Local> = start.into();
        assert_eq!(
            start_dt.format("%Y%m%d_%H%M%S").to_string(),
            "20260101_120000"
        );
        assert!(session_started_at(logs_dir.join("nope.log").as_path()).is_none());

        let newest = newest_session_for_instance(&dir, "Inst").expect("newest run");
        assert_eq!(newest, new, "per-instance newest, tees excluded");
        assert!(newest_session_for_instance(&dir, "Nope").is_none());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn newest_session_is_scoped_to_each_running_instance() {
        let dir = std::env::temp_dir().join(format!(
            "vl_gamelogs_two_games_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let logs_dir = dir.join("logs").join("game");
        std::fs::create_dir_all(&logs_dir).unwrap();

        let alpha_old = logs_dir.join("Alpha_20260101_120000.log");
        let alpha_new = logs_dir.join("Alpha_20260102_120000.log");
        let beta = logs_dir.join("Beta_20260103_120000.log");
        std::fs::write(&alpha_old, "old\n").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(15));
        std::fs::write(&alpha_new, "new\n").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(15));
        std::fs::write(&beta, "beta\n").unwrap();

        assert_eq!(
            newest_session_for_instance(&dir, "Alpha"),
            Some(alpha_new),
            "the other running game must not win"
        );
        assert_eq!(newest_session_for_instance(&dir, "Beta"), Some(beta));
        assert!(newest_session_for_instance(&dir, "Gamma").is_none());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Manual crash analysis passes the listed instance_name to the
    /// instance lookup, so it must be the REAL name from the log header —
    /// the sanitized filename stem can never be resolved back to an
    /// instance directory.
    #[test]
    fn session_instance_name_prefers_real_header_over_sanitized_filename() {
        let dir = std::env::temp_dir().join(format!("vl_gamelogs_name_{}", std::process::id()));
        let logs_dir = dir.join("logs").join("game");
        std::fs::create_dir_all(&logs_dir).unwrap();

        std::fs::write(
            logs_dir.join("Better_MC__FORGE__BMC4_20261018_190854.log"),
            "VoidLauncher Game Log\nInstance: Better MC [FORGE] BMC4\nStarted: 18/10/2026 19:08:54\n====\n",
        )
        .unwrap();
        // Header-less file keeps the filename-derived fallback.
        std::fs::write(logs_dir.join("Plain_20260101_000000.log"), "no header\n").unwrap();

        let sessions = list_game_log_sessions(&dir);
        let real = sessions
            .iter()
            .find(|s| {
                s.path
                    .ends_with("Better_MC__FORGE__BMC4_20261018_190854.log")
            })
            .expect("session listed");
        assert_eq!(real.instance_name, "Better MC [FORGE] BMC4");

        let plain = sessions
            .iter()
            .find(|s| s.path.ends_with("Plain_20260101_000000.log"))
            .expect("session listed");
        assert_eq!(plain.instance_name, "Plain");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_game_log_returns_last_n_lines() {
        let dir = std::env::temp_dir().join(format!("vl_readlog_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session.log");
        let lines: Vec<String> = (1..=100).map(|i| format!("line {}", i)).collect();
        std::fs::write(&path, lines.join("\n")).unwrap();

        let tail = read_game_log(path.to_str().unwrap(), Some(5)).unwrap();
        assert!(tail.contains("line 96"));
        assert!(tail.contains("line 100"));
        assert!(tail.ends_with("line 100"));
        assert!(tail.starts_with("... (showing last 5 lines)"));
        // Header plus exactly 5 data lines.
        assert_eq!(tail.matches('\n').count(), 5);

        // Larger cap than the file returns everything.
        let all = read_game_log(path.to_str().unwrap(), Some(500)).unwrap();
        assert!(all.starts_with("line 1"));
        assert!(all.contains("line 100"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_game_log_tail_spans_multiple_blocks_and_trailing_newline() {
        let dir = std::env::temp_dir().join(format!("vl_readlog_blocks_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("big.log");

        // Produce >2× the 64 KiB block so the backward scan crosses block
        // boundaries, and end the file with a trailing newline.
        let mut content = String::new();
        for _ in 0..300_000 {
            content.push_str("0123456789abcdef\n");
        }
        std::fs::write(&path, &content).unwrap();

        let tail = read_game_log(path.to_str().unwrap(), Some(7)).unwrap();
        let kept: Vec<&str> = tail.lines().collect();
        // First element is the truncation header; the rest are the 7 newest
        // visible lines (the file's trailing \n must not add an empty line).
        assert_eq!(kept.len(), 8, "header + exactly 7 lines");
        assert!(kept[0].starts_with("... (showing last 7 lines)"));
        for l in &kept[1..] {
            assert_eq!(*l, "0123456789abcdef");
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_read_game_log_incremental_incomplete_line_and_utf8_safety() {
        let temp =
            std::env::temp_dir().join(format!("voidlauncher-inc-log-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp).unwrap();
        let log_file = temp.join("session.log");

        // 1. Write initial line
        std::fs::write(&log_file, "Line 1: Started\n").unwrap();
        let c1 = read_game_log_incremental(log_file.to_str().unwrap(), 0, None, None).unwrap();
        assert_eq!(c1.lines, vec!["Line 1: Started"]);
        assert!(c1.reset);

        // 2. Append incomplete line with Cyrillic (no trailing newline)
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&log_file)
            .unwrap();
        use std::io::Write;
        // "Ошибка: " without \n
        f.write_all("Ошибка: ".as_bytes()).unwrap();
        drop(f);

        // Incremental read should NOT cut or consume the incomplete line
        let c2 = read_game_log_incremental(
            log_file.to_str().unwrap(),
            c1.new_byte_offset,
            None,
            Some(&c1.file_token),
        )
        .unwrap();
        assert!(
            c2.lines.is_empty(),
            "Incomplete line without newline must not be yielded"
        );
        assert_eq!(
            c2.new_byte_offset, c1.new_byte_offset,
            "Offset must not advance past incomplete line"
        );

        // 3. Complete the line with Cyrillic and trailing newline
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&log_file)
            .unwrap();
        f.write_all("сбой сети\n".as_bytes()).unwrap();
        drop(f);

        // Incremental read now yields the entire complete line cleanly without UTF-8 corruption
        let c3 = read_game_log_incremental(
            log_file.to_str().unwrap(),
            c2.new_byte_offset,
            None,
            Some(&c2.file_token),
        )
        .unwrap();
        assert_eq!(c3.lines, vec!["Ошибка: сбой сети"]);
        assert!(!c3.reset);

        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn test_read_game_log_incremental_reset_does_not_split_partial_last_line() {
        let temp =
            std::env::temp_dir().join(format!("voidlauncher-inc-partial-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp).unwrap();
        let log_file = temp.join("session.log");
        std::fs::write(&log_file, "line1\npartial").unwrap();

        let c1 = read_game_log_incremental(log_file.to_str().unwrap(), 0, None, None).unwrap();
        assert!(c1.reset);
        assert_eq!(c1.lines, vec!["line1"]);
        assert_eq!(
            c1.new_byte_offset,
            "line1\n".len() as u64,
            "offset must stop after the last complete line"
        );

        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&log_file)
                .unwrap();
            f.write_all(b" rest\n").unwrap();
            f.flush().unwrap();
        }
        let c2 = read_game_log_incremental(
            log_file.to_str().unwrap(),
            c1.new_byte_offset,
            None,
            Some(&c1.file_token),
        )
        .unwrap();
        assert!(!c2.reset);
        assert_eq!(c2.lines, vec!["partial rest"]);

        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn test_read_game_log_incremental_appended_and_reset() {
        let dir = std::env::temp_dir().join(format!("vl_incr_log_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session.log");

        // 1. Initial write of 3 lines
        std::fs::write(&path, "line 1\nline 2\nline 3\n").unwrap();

        let chunk1 = read_game_log_incremental(path.to_str().unwrap(), 0, Some(50), None).unwrap();
        assert!(chunk1.reset);
        assert_eq!(chunk1.lines, vec!["line 1", "line 2", "line 3"]);
        assert!(chunk1.new_byte_offset > 0);

        // 2. No changes
        let chunk2 = read_game_log_incremental(
            path.to_str().unwrap(),
            chunk1.new_byte_offset,
            Some(50),
            Some(&chunk1.file_token),
        )
        .unwrap();
        assert!(!chunk2.reset);
        assert!(chunk2.lines.is_empty());
        assert_eq!(chunk2.new_byte_offset, chunk1.new_byte_offset);

        // 3. Append 2 lines
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(b"line 4\nline 5\n").unwrap();
        drop(file);

        let chunk3 = read_game_log_incremental(
            path.to_str().unwrap(),
            chunk1.new_byte_offset,
            Some(50),
            Some(&chunk1.file_token),
        )
        .unwrap();
        assert!(!chunk3.reset);
        assert_eq!(chunk3.lines, vec!["line 4", "line 5"]);
        assert!(chunk3.new_byte_offset > chunk1.new_byte_offset);

        // 4. Truncation / reset
        std::fs::write(&path, "fresh line\n").unwrap();
        let chunk4 = read_game_log_incremental(
            path.to_str().unwrap(),
            chunk3.new_byte_offset,
            Some(50),
            Some(&chunk3.file_token),
        )
        .unwrap();
        assert!(chunk4.reset);
        assert_eq!(chunk4.lines, vec!["fresh line"]);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_read_game_log_incremental_detects_replacement_by_larger_file() {
        let dir = std::env::temp_dir().join(format!("vl_incr_replace_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let log_path = dir.join("latest.log");

        // 1. File 1: small file
        std::fs::write(&log_path, "Run 1: line 1\nRun 1: line 2\n").unwrap();
        let chunk1 = read_game_log_incremental(log_path.to_str().unwrap(), 0, None, None).unwrap();
        assert!(chunk1.reset);
        assert_eq!(chunk1.lines, vec!["Run 1: line 1", "Run 1: line 2"]);
        let offset1 = chunk1.new_byte_offset;
        let token1 = chunk1.file_token.clone();
        assert!(!token1.is_empty());

        // 2. Replace file 1 with file 2 that is SUBSTANTIALLY LARGER than offset1
        // (so file_len > byte_offset is strictly true, which would fool a naive length check)
        let large_run2 =
            "Run 2: this is a completely new session with lots of initialization data\n".repeat(10);
        std::fs::write(&log_path, &large_run2).unwrap();
        let file_len2 = std::fs::metadata(&log_path).unwrap().len();
        assert!(
            file_len2 > offset1,
            "New file must be larger than previous offset"
        );

        // 3. Read incrementally passing previous offset1 and token1
        let chunk2 =
            read_game_log_incremental(log_path.to_str().unwrap(), offset1, None, Some(&token1))
                .unwrap();

        // 4. Verification: file replacement was detected via token mismatch!
        assert!(
            chunk2.reset,
            "Replacement by larger file must trigger reset!"
        );
        assert_ne!(
            chunk2.file_token, token1,
            "File token must change for replaced file"
        );
        assert_eq!(chunk2.lines.len(), 10);
        assert_eq!(
            chunk2.lines[0],
            "Run 2: this is a completely new session with lots of initialization data"
        );
        assert_eq!(chunk2.new_byte_offset, file_len2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_read_game_log_incremental_detects_in_place_rewrite_with_same_first_line() {
        let dir = std::env::temp_dir().join(format!("vl_in_place_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let log_path = dir.join("latest.log");

        // 1. Initial write: file with header and first lines
        let header = "VoidLauncher Game Log - Session Header\n";
        let initial_content = format!("{}Old run line 2\nOld run line 3\n", header);
        std::fs::write(&log_path, &initial_content).unwrap();

        // Read initial content
        let chunk1 = read_game_log_incremental(log_path.to_str().unwrap(), 0, None, None).unwrap();
        assert!(chunk1.reset);
        assert_eq!(chunk1.lines.len(), 3);
        assert_eq!(chunk1.lines[0], "VoidLauncher Game Log - Session Header");
        let offset1 = chunk1.new_byte_offset;
        let token1 = chunk1.file_token.clone();

        // 2. Overwrite the EXACT SAME file object in place (truncate = true)
        // Keep the exact same first line (header), but completely rewrite subsequent lines
        // with content that makes the new file size substantially LARGER than offset1.
        let mut rewritten_content = header.to_string();
        for i in 1..=10 {
            rewritten_content.push_str(&format!("Completely fresh run session line {}\n", i));
        }
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&log_path)
                .unwrap();
            f.write_all(rewritten_content.as_bytes()).unwrap();
            f.flush().unwrap();
        }

        let new_len = std::fs::metadata(&log_path).unwrap().len();
        assert!(
            new_len > offset1,
            "New file must be larger than old offset1"
        );

        // 3. Read incrementally passing old offset1 and token1
        let chunk2 =
            read_game_log_incremental(log_path.to_str().unwrap(), offset1, None, Some(&token1))
                .unwrap();

        // 4. Assert that in-place rewrite is detected and resets the stream
        assert!(
            chunk2.reset,
            "In-place rewrite with same header must trigger reset!"
        );
        assert_ne!(
            chunk2.file_token, token1,
            "Token must change when past content is rewritten"
        );
        assert_eq!(chunk2.lines.len(), 11);
        assert_eq!(chunk2.lines[0], "VoidLauncher Game Log - Session Header");
        assert_eq!(chunk2.lines[1], "Completely fresh run session line 1");
        assert_eq!(chunk2.new_byte_offset, new_len);

        // 5. Subsequent append to the new file does NOT reset
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&log_path)
                .unwrap();
            f.write_all(b"Appended line 12\n").unwrap();
            f.flush().unwrap();
        }
        let chunk3 = read_game_log_incremental(
            log_path.to_str().unwrap(),
            chunk2.new_byte_offset,
            None,
            Some(&chunk2.file_token),
        )
        .unwrap();
        assert!(!chunk3.reset, "Subsequent normal append must NOT reset");
        assert_eq!(chunk3.lines, vec!["Appended line 12"]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_read_game_log_incremental_detects_rewrite_in_middle_of_file() {
        let dir = std::env::temp_dir().join(format!("vl_mid_rewrite_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let log_path = dir.join("latest.log");

        // 1. Create a substantial log file: e.g. 4000 bytes
        // Header line (50 bytes)
        let header = "VoidLauncher Game Log - Header Line Prefix Info\n";
        // Fill prefix (256 bytes total with header)
        let mut prefix = header.to_string();
        while prefix.len() < 256 {
            prefix.push_str("P_PAD_LINE_HEADER_DATA_1234567890\n");
        }
        let prefix_bytes = prefix.as_bytes()[..256].to_vec();

        // Middle section (e.g. 2000 bytes)
        let middle_original = "ORIGINAL_MIDDLE_DATA_LINE_ENTRY_00000000000000000\n".repeat(40);

        // Tail section (e.g. 100 bytes ending at byte_offset)
        let tail =
            "SUFFIX_DATA_PRESERVED_LAST_64_BYTES_LINE_ENTRY_END_001234567890\nSUFFIX_PAD_DATA\n";
        assert!(tail.len() >= 64);

        let mut initial_file = Vec::new();
        initial_file.extend_from_slice(&prefix_bytes);
        initial_file.extend_from_slice(middle_original.as_bytes());
        initial_file.extend_from_slice(tail.as_bytes());

        let target_offset = initial_file.len() as u64;

        // Write initial file and read it
        std::fs::write(&log_path, &initial_file).unwrap();
        let chunk1 = read_game_log_incremental(log_path.to_str().unwrap(), 0, None, None).unwrap();
        assert!(chunk1.reset);
        assert_eq!(chunk1.new_byte_offset, target_offset);
        let token1 = chunk1.file_token.clone();

        // 2. Overwrite the SAME file object in place (truncate = true)
        // Strictly preserve:
        // - file identity (same path, in-place write)
        // - first line
        // - first 256 bytes (prefix_bytes)
        // - last 64 bytes before target_offset
        // Change ONLY the data strictly in between!
        let middle_modified = "MODIFIED_TAMPERED_MIDDLE_DATA_LINE_ZZZZZZZZZZZZZZ\n".repeat(40);
        assert_eq!(middle_original.len(), middle_modified.len());

        let mut rewritten_file = Vec::new();
        rewritten_file.extend_from_slice(&prefix_bytes);
        rewritten_file.extend_from_slice(middle_modified.as_bytes());
        rewritten_file.extend_from_slice(tail.as_bytes());
        assert_eq!(rewritten_file.len() as u64, target_offset);

        // Make the new file LARGER than target_offset
        rewritten_file.extend_from_slice(
            b"EXTRA_NEW_DATA_BEYOND_OLD_OFFSET_LINE_001\nEXTRA_NEW_DATA_LINE_002\n",
        );
        let new_len = rewritten_file.len() as u64;
        assert!(new_len > target_offset);

        // Verify that the test setup strictly preserved the first 256 bytes and last 64 bytes before target_offset
        assert_eq!(&rewritten_file[..256], &initial_file[..256]);
        assert_eq!(
            &rewritten_file[target_offset as usize - 64..target_offset as usize],
            &initial_file[target_offset as usize - 64..target_offset as usize]
        );

        // Overwrite in place
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&log_path)
                .unwrap();
            f.write_all(&rewritten_file).unwrap();
            f.flush().unwrap();
        }

        // 3. Read incrementally passing old target_offset and token1
        let chunk2 = read_game_log_incremental(
            log_path.to_str().unwrap(),
            target_offset,
            None,
            Some(&token1),
        )
        .unwrap();

        // 4. Assert that change between checked regions is detected and triggers reset!
        assert!(
            chunk2.reset,
            "Modification in middle of file must trigger reset!"
        );
        assert_ne!(
            chunk2.file_token, token1,
            "Token must change when middle data is modified"
        );
        assert_eq!(chunk2.new_byte_offset, new_len);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_read_game_log_incremental_large_file_checkpoint_sampling_detects_modification() {
        let dir = std::env::temp_dir().join(format!("vl_large_cp_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let log_path = dir.join("large_session.log");

        // 1. Build a log file strictly > 256 KB (e.g. 300_000 bytes)
        let header = "VoidLauncher Game Log - Large File Checkpoint Test\n";
        let mut initial_data = Vec::with_capacity(300_000);
        initial_data.extend_from_slice(header.as_bytes());

        // Fill up to exactly 300_000 bytes with lines
        let fill_line = b"[INFO] Regular game log entry data line 0123456789abcdef\n";
        while initial_data.len() + fill_line.len() <= 300_000 {
            initial_data.extend_from_slice(fill_line);
        }
        while initial_data.len() < 300_000 {
            initial_data.push(b'\n');
        }
        let target_offset = 300_000u64;
        assert_eq!(initial_data.len() as u64, target_offset);
        assert!(
            target_offset > 256 * 1024,
            "File must exceed 256 KB threshold"
        );

        std::fs::write(&log_path, &initial_data).unwrap();
        let chunk1 = read_game_log_incremental(log_path.to_str().unwrap(), 0, None, None).unwrap();
        assert!(chunk1.reset);
        assert_eq!(chunk1.new_byte_offset, target_offset);
        let token1 = chunk1.file_token.clone();

        // 2. Overwrite the SAME file in place:
        // - Preserve head (first 4096 bytes)
        // - Preserve tail (last 4096 bytes before target_offset)
        // - Alter bytes at checkpoint 8 (approx offset 141_416)
        // - Append extra bytes so new_len > target_offset
        let middle_start = 4096u64;
        let tail_start = target_offset - 4096;
        let step = (tail_start - middle_start) / 17;
        let cp8_pos = (middle_start + step * 8) as usize;

        let mut modified_data = initial_data.clone();
        for i in 0..256 {
            modified_data[cp8_pos + i] = b'Z';
        }
        // Extra data beyond target_offset
        modified_data.extend_from_slice(b"[INFO] Extra appended data beyond target_offset\n");
        let new_len = modified_data.len() as u64;
        assert!(new_len > target_offset);

        // Verify head and tail were preserved
        assert_eq!(&modified_data[..4096], &initial_data[..4096]);
        assert_eq!(
            &modified_data[tail_start as usize..target_offset as usize],
            &initial_data[tail_start as usize..target_offset as usize]
        );

        // In-place rewrite
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&log_path)
                .unwrap();
            f.write_all(&modified_data).unwrap();
            f.flush().unwrap();
        }

        // 3. Read incrementally passing old target_offset and token1
        let chunk2 = read_game_log_incremental(
            log_path.to_str().unwrap(),
            target_offset,
            None,
            Some(&token1),
        )
        .unwrap();

        // 4. Verification: checkpoint sampling detected the modification and triggered reset
        assert!(
            chunk2.reset,
            "Modification in sampled checkpoint must trigger reset"
        );
        assert_ne!(
            chunk2.file_token, token1,
            "Token must change when sampled checkpoint is altered"
        );
        assert_eq!(chunk2.new_byte_offset, new_len);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_read_game_log_incremental_large_file_append_only_does_not_reset() {
        let dir = std::env::temp_dir().join(format!("vl_large_app_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let log_path = dir.join("large_append.log");

        // 1. Build a log file strictly > 256 KB (e.g. 300_000 bytes)
        let header = "VoidLauncher Game Log - Large Append Test\n";
        let mut initial_data = Vec::with_capacity(300_000);
        initial_data.extend_from_slice(header.as_bytes());

        let fill_line = b"[INFO] Large game session regular line entry 0123456789\n";
        while initial_data.len() + fill_line.len() <= 300_000 {
            initial_data.extend_from_slice(fill_line);
        }
        while initial_data.len() < 300_000 {
            initial_data.push(b'\n');
        }
        let target_offset = 300_000u64;
        assert_eq!(initial_data.len() as u64, target_offset);
        assert!(
            target_offset > 256 * 1024,
            "File must exceed 256 KB threshold"
        );

        std::fs::write(&log_path, &initial_data).unwrap();

        // Initial read
        let chunk1 = read_game_log_incremental(log_path.to_str().unwrap(), 0, None, None).unwrap();
        assert!(chunk1.reset);
        assert_eq!(chunk1.new_byte_offset, target_offset);
        let token1 = chunk1.file_token.clone();

        // 2. Normal append-only write of new lines at the end of the file
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&log_path)
                .unwrap();
            f.write_all(b"[INFO] Freshly appended line 1 in large session\n")
                .unwrap();
            f.write_all(b"[INFO] Freshly appended line 2 in large session\n")
                .unwrap();
            f.flush().unwrap();
        }

        // 3. Read incrementally with target_offset and token1
        let chunk2 = read_game_log_incremental(
            log_path.to_str().unwrap(),
            target_offset,
            None,
            Some(&token1),
        )
        .unwrap();

        // 4. Verification: NO false reset; lines smoothly yielded
        assert!(
            !chunk2.reset,
            "Append-only growth must NOT trigger reset for large files"
        );
        assert_eq!(
            chunk2.lines,
            vec![
                "[INFO] Freshly appended line 1 in large session",
                "[INFO] Freshly appended line 2 in large session",
            ]
        );
        assert!(chunk2.new_byte_offset > target_offset);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_huge_incremental_delta_is_capped_and_later_reads_stay_correct() {
        let dir = std::env::temp_dir().join(format!("vl_log_cap_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let log_path = dir.join("cap.log");
        std::fs::write(&log_path, "start\n").unwrap();

        let first = read_game_log_incremental(log_path.to_str().unwrap(), 0, None, None).unwrap();
        assert!(first.reset);

        // Append well over the per-poll cap in one burst, ending with
        // recognizable lines.
        let filler = b"[INFO] filler log line 0123456789abcdef\n";
        let mut burst = Vec::new();
        while burst.len() as u64 <= MAX_INCREMENTAL_LOG_BYTES + 512 * 1024 {
            burst.extend_from_slice(filler);
        }
        burst.extend_from_slice(b"[INFO] LAST-A\n[INFO] LAST-B\n");
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&log_path)
                .unwrap();
            f.write_all(&burst).unwrap();
            f.flush().unwrap();
        }
        let file_len = std::fs::metadata(&log_path).unwrap().len();

        let second = read_game_log_incremental(
            log_path.to_str().unwrap(),
            first.new_byte_offset,
            None,
            Some(&first.file_token),
        )
        .unwrap();
        assert!(!second.reset, "a large append is not a rotation");
        assert!(
            second.new_byte_offset > first.new_byte_offset,
            "the offset must advance past the dropped middle"
        );
        assert!(second.new_byte_offset <= file_len);
        assert!(
            second
                .lines
                .last()
                .is_some_and(|line| line.contains("LAST-B")),
            "the newest lines must survive the cap: {:?}",
            second.lines.last()
        );
        assert!(
            second.lines.len() < (MAX_INCREMENTAL_LOG_BYTES / 8) as usize,
            "the returned chunk must respect the byte cap"
        );

        // Reading continues correctly after the capped chunk.
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&log_path)
                .unwrap();
            f.write_all(b"[INFO] AFTER-CAP\n").unwrap();
            f.flush().unwrap();
        }
        let third = read_game_log_incremental(
            log_path.to_str().unwrap(),
            second.new_byte_offset,
            None,
            Some(&second.file_token),
        )
        .unwrap();
        assert_eq!(third.lines, vec!["[INFO] AFTER-CAP"]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_read_game_log_incremental_large_file_sequential_polling_stability() {
        let dir = std::env::temp_dir().join(format!("vl_seq_poll_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let log_path = dir.join("seq_poll.log");

        // 1. Create a large file (> 256 KB, e.g. 300_000 bytes)
        let header = "VoidLauncher Game Log - Sequential Polling Test\n";
        let mut initial_data = Vec::with_capacity(300_000);
        initial_data.extend_from_slice(header.as_bytes());

        let fill_line = b"[INFO] Base game session log line 0123456789abcdefghijklmnopqrst\n";
        while initial_data.len() + fill_line.len() <= 300_000 {
            initial_data.extend_from_slice(fill_line);
        }
        while initial_data.len() < 300_000 {
            initial_data.push(b'\n');
        }
        let target_offset = 300_000u64;
        assert_eq!(initial_data.len() as u64, target_offset);
        assert!(
            target_offset > 256 * 1024,
            "File must exceed 256 KB threshold"
        );

        std::fs::write(&log_path, &initial_data).unwrap();

        // Step 1: Initial read of large file
        let chunk1 = read_game_log_incremental(log_path.to_str().unwrap(), 0, None, None).unwrap();
        assert!(chunk1.reset);
        assert_eq!(chunk1.new_byte_offset, target_offset);
        let offset1 = chunk1.new_byte_offset;
        let token1 = chunk1.file_token.clone();

        // Step 2: Append several lines and perform incremental read (Poll 2)
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&log_path)
                .unwrap();
            f.write_all(b"[INFO] Poll 2 appended line A\n").unwrap();
            f.write_all(b"[INFO] Poll 2 appended line B\n").unwrap();
            f.flush().unwrap();
        }
        let chunk2 =
            read_game_log_incremental(log_path.to_str().unwrap(), offset1, None, Some(&token1))
                .unwrap();
        assert!(!chunk2.reset, "Poll 2 must NOT trigger reset");
        assert_eq!(
            chunk2.lines,
            vec![
                "[INFO] Poll 2 appended line A",
                "[INFO] Poll 2 appended line B"
            ]
        );
        let offset2 = chunk2.new_byte_offset;
        let token2 = chunk2.file_token.clone();
        assert!(offset2 > offset1);

        // Step 3 & 4: Next poll without ANY changes to file (Poll 3)
        let chunk3 =
            read_game_log_incremental(log_path.to_str().unwrap(), offset2, None, Some(&token2))
                .unwrap();
        assert!(
            !chunk3.reset,
            "Poll 3 without changes must NOT trigger reset"
        );
        assert!(
            chunk3.lines.is_empty(),
            "Poll 3 without changes must return empty lines"
        );
        assert_eq!(chunk3.new_byte_offset, offset2);
        let offset3 = chunk3.new_byte_offset;
        let token3 = chunk3.file_token.clone();
        assert_eq!(
            token3, token2,
            "Token must remain identical when file did not change"
        );

        // Step 5: Append more lines and perform third read (Poll 4)
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&log_path)
                .unwrap();
            f.write_all(b"[INFO] Poll 4 appended line C\n").unwrap();
            f.write_all(b"[INFO] Poll 4 appended line D\n").unwrap();
            f.flush().unwrap();
        }
        let chunk4 =
            read_game_log_incremental(log_path.to_str().unwrap(), offset3, None, Some(&token3))
                .unwrap();
        assert!(
            !chunk4.reset,
            "Poll 4 with new lines must NOT trigger reset"
        );
        assert_eq!(
            chunk4.lines,
            vec![
                "[INFO] Poll 4 appended line C",
                "[INFO] Poll 4 appended line D"
            ]
        );
        assert!(chunk4.new_byte_offset > offset3);

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- output reader completion (smoke diagnostics) ---------------------

    /// A stream that yields `lines` and then blocks until `release` is set,
    /// so a test controls exactly when EOF happens.
    struct GatedReader {
        lines: std::collections::VecDeque<String>,
        gate: Arc<(Mutex<bool>, Condvar)>,
    }

    impl std::io::Read for GatedReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if let Some(line) = self.lines.pop_front() {
                let bytes = format!("{}\n", line).into_bytes();
                buf[..bytes.len()].copy_from_slice(&bytes);
                return Ok(bytes.len());
            }
            let (lock, cv) = &*self.gate;
            let mut released = lock.lock().expect("gate");
            while !*released {
                released = cv.wait(released).expect("gate wait");
            }
            Ok(0)
        }
    }

    fn gated(lines: &[&str]) -> (GatedReader, Arc<(Mutex<bool>, Condvar)>) {
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        (
            GatedReader {
                lines: lines.iter().map(|s| s.to_string()).collect(),
                gate: gate.clone(),
            },
            gate,
        )
    }

    fn release(gate: &Arc<(Mutex<bool>, Condvar)>) {
        let (lock, cv) = &**gate;
        *lock.lock().expect("gate") = true;
        cv.notify_all();
    }

    fn reader_temp_dir(prefix: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("vl_readers_{}_{}", prefix, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn wait_for_readers_blocks_until_both_streams_end_and_never_hangs() {
        let dir = reader_temp_dir("wait");
        let out_path = dir.join("game.stdout.log");
        let err_path = dir.join("game.stderr.log");
        let (out, out_gate) = gated(&["java: line from stdout"]);
        let (err, err_gate) = gated(&["java: line from stderr"]);
        let pid = 424_242;
        attach_output_readers(
            pid,
            Box::new(out),
            Box::new(err),
            std::fs::File::create(&out_path).ok(),
            std::fs::File::create(&err_path).ok(),
            None,
        );

        // A stalled reader must not hang the caller: the bounded wait ends.
        assert_eq!(
            wait_for_output_readers(pid, Duration::from_millis(300)),
            ReadersWait::TimedOut
        );

        release(&out_gate);
        release(&err_gate);
        assert_eq!(
            wait_for_output_readers(pid, Duration::from_secs(5)),
            ReadersWait::Finished
        );

        // EOF + flush is reported only after the last lines are on disk, so
        // the files can be read immediately after the wait.
        let out_text = std::fs::read_to_string(&out_path).expect("stdout tee");
        let err_text = std::fs::read_to_string(&err_path).expect("stderr tee");
        assert!(out_text.contains("java: line from stdout"), "{out_text}");
        assert!(err_text.contains("java: line from stderr"), "{err_text}");
        assert!(
            out_text.ends_with("java: line from stdout\n"),
            "the last line must be flushed before completion: {out_text:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wait_for_readers_reports_unknown_for_an_unattached_pid() {
        assert_eq!(
            wait_for_output_readers(999_999, Duration::from_millis(50)),
            ReadersWait::Unknown
        );
    }

    #[test]
    fn last_lines_survive_process_end_even_with_heavy_output() {
        let dir = reader_temp_dir("tail");
        let out_path = dir.join("game.stdout.log");
        let mut lines: Vec<String> = (0..500).map(|i| format!("line-{:04}", i)).collect();
        let last = lines.last().cloned().expect("lines");
        let mut out = std::collections::VecDeque::new();
        out.extend(lines.drain(..));
        let gate = Arc::new((Mutex::new(true), Condvar::new()));
        let pid = 424_243;
        attach_output_readers(
            pid,
            Box::new(GatedReader { lines: out, gate }),
            Box::new(std::io::Cursor::new(Vec::new())),
            std::fs::File::create(&out_path).ok(),
            None,
            None,
        );
        assert!(
            matches!(
                wait_for_output_readers(pid, Duration::from_secs(5)),
                ReadersWait::Finished | ReadersWait::Unknown
            ),
            "the run must complete"
        );
        let text = std::fs::read_to_string(&out_path).expect("stdout tee");
        assert!(
            text.contains(&last),
            "the final line is missing: {:?}",
            text.lines().last()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

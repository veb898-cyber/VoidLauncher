use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::process::Child;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Per-instance playtime record (in minutes)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlaytimeEntry {
    /// Whole minutes played (the smallest unit we commit to disk)
    #[serde(default)]
    pub minutes: u64,
}

/// JSON-backed map of instance name -> playtime entry
pub type PlaytimeMap = HashMap<String, PlaytimeEntry>;

/// Load the playtime map from `data_dir/playtime.json`. Returns empty map on any error.
pub fn load_playtime(data_dir: &Path) -> PlaytimeMap {
    let path = data_dir.join("playtime.json");
    let Ok(contents) = fs::read_to_string(&path) else { return HashMap::new(); };
    serde_json::from_str(&contents).unwrap_or_default()
}

/// Persist the playtime map atomically (write to temp then rename). Best-effort; ignores errors.
pub fn save_playtime(data_dir: &Path, map: &PlaytimeMap) {
    let _ = fs::create_dir_all(data_dir);
    let final_path = data_dir.join("playtime.json");
    let tmp_path = data_dir.join("playtime.json.tmp");
    if let Ok(json) = serde_json::to_string_pretty(map) {
        if fs::write(&tmp_path, json).is_ok() {
            let _ = fs::rename(&tmp_path, &final_path);
        }
    }
}

/// Output language for `format_playtime`. Passed from the frontend's
/// language store so the playtime label always matches the UI locale.
///
/// The `Ru` variant is kept for back-compat (see `format_playtime`) even
/// though the live launcher UI is English-only and never constructs it.
/// Marked `#[allow(dead_code)]` to silence the warning while preserving
/// the Russian test coverage in the test module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaytimeLang {
    En,
    #[allow(dead_code)]
    Ru,
}

/// Format minutes as a human-readable, locale-aware string.
/// Examples (en): `format_playtime(3630) = "60 hours 30 minutes"`.
/// Examples (ru): `format_playtime(3630) = "60 часов 30 минут"`.
pub fn format_playtime_in(minutes: u64, lang: PlaytimeLang) -> String {
    if minutes == 0 {
        return match lang {
            PlaytimeLang::En => "0 minutes".to_string(),
            PlaytimeLang::Ru => "0 минут".to_string(),
        };
    }
    let h = minutes / 60;
    let m = minutes % 60;
    let mut out = String::new();
    if h > 0 {
        out.push_str(&h.to_string());
        out.push(' ');
        out.push_str(plural_hours(h, lang));
    }
    if m > 0 {
        if h > 0 { out.push(' '); }
        out.push_str(&m.to_string());
        out.push(' ');
        out.push_str(plural_minutes(m, lang));
    }
    out
}

/// Back-compat wrapper: Russian locale (the historical default before
/// the language selector existed). Prefer `format_playtime_in(_, _)`.
/// Marked `#[allow(dead_code)]` because nothing in the current codebase
/// calls it — only the test module exercises the Russian locale to
/// lock the back-compat contract.
#[allow(dead_code)]
pub fn format_playtime(minutes: u64) -> String {
    format_playtime_in(minutes, PlaytimeLang::Ru)
}

fn plural_hours(n: u64, lang: PlaytimeLang) -> &'static str {
    match lang {
        PlaytimeLang::En => if n == 1 { "hour" } else { "hours" },
        PlaytimeLang::Ru => {
            let mod10 = n % 10;
            let mod100 = n % 100;
            if mod10 == 1 && mod100 != 11 { "час" }
            else if (2..=4).contains(&mod10) && !(12..=14).contains(&mod100) { "часа" }
            else { "часов" }
        }
    }
}

fn plural_minutes(n: u64, lang: PlaytimeLang) -> &'static str {
    match lang {
        PlaytimeLang::En => if n == 1 { "minute" } else { "minutes" },
        PlaytimeLang::Ru => {
            let mod10 = n % 10;
            let mod100 = n % 100;
            if mod10 == 1 && mod100 != 11 { "минуту" }
            else if (2..=4).contains(&mod10) && !(12..=14).contains(&mod100) { "минуты" }
            else { "минут" }
        }
    }
}

/// Handle to an active tracked Minecraft session.
///
/// The child process is owned by `child` (Mutex-protected) so the timer task can
/// poll it via `try_wait()`. `last_flush` tracks the last time we committed a
/// minute to disk — we use it to compute the "unpaid remainder" on close/flush.
pub struct ActiveSession {
    pub instance_name: String,
    pub pid: u32,
    pub started_at: Instant,
    pub last_flush: Instant,
    pub child: Arc<Mutex<Option<Child>>>,
}

impl ActiveSession {
    /// Returns the number of whole minutes elapsed since `last_flush` (without committing).
    pub fn unpaid_minutes(&self, now: Instant) -> u64 {
        let elapsed = now.duration_since(self.last_flush);
        (elapsed.as_secs() / 60) as u64
    }

    /// Compute the remaining unpaid seconds (sub-minute remainder) since `last_flush`.
    pub fn unpaid_seconds(&self, now: Instant) -> u64 {
        let elapsed = now.duration_since(self.last_flush).as_secs();
        elapsed % 60
    }

    /// Check if the underlying process is still alive. Returns false if dead or already reaped.
    pub fn is_alive(&self) -> bool {
        if let Ok(mut guard) = self.child.lock() {
            if let Some(child) = guard.as_mut() {
                match child.try_wait() {
                    Ok(None) => return true,
                    Ok(Some(_)) | Err(_) => return false,
                }
            }
        }
        false
    }
}

/// Helper to update the playtime map and persist it (used by the timer + close handler).
pub fn add_minutes_and_save(data_dir: &Path, instance_name: &str, delta_minutes: u64) {
    if delta_minutes == 0 { return; }
    let mut map = load_playtime(data_dir);
    let entry = map.entry(instance_name.to_string()).or_default();
    entry.minutes = entry.minutes.saturating_add(delta_minutes);
    save_playtime(data_dir, &map);
}

/// Get total playtime in minutes for a single instance.
pub fn get_playtime(data_dir: &Path, instance_name: &str) -> u64 {
    let map = load_playtime(data_dir);
    map.get(instance_name).map(|e| e.minutes).unwrap_or(0)
}

/// Take the session for a single instance out of a multi-instance map
/// (returns instance name + unpaid minutes delta). Returns None if that
/// instance has no active session.
pub fn take_keyed_session(
    sessions: &Mutex<HashMap<String, ActiveSession>>,
    instance_name: &str,
    now: Instant,
) -> Option<(String, u64)> {
    let mut guard = sessions.lock().ok()?;
    let s = guard.remove(instance_name)?;
    let delta = s.unpaid_minutes(now);
    Some((instance_name.to_string(), delta))
}

/// Take and commit all active sessions (used on window close). Returns the
/// total number of flushed sessions for logging.
pub fn flush_all_sessions(
    sessions: &Mutex<HashMap<String, ActiveSession>>,
    data_dir: &Path,
    now: Instant,
) -> usize {
    let mut guards = HashMap::new();
    {
        if let Ok(mut map) = sessions.lock() {
            for (name, s) in map.drain() {
                let delta = s.unpaid_minutes(now);
                if delta > 0 {
                    guards.insert(name, delta);
                }
            }
        }
    }
    let count = guards.len();
    for (name, delta) in guards {
        add_minutes_and_save(data_dir, &name, delta);
    }
    count
}

/// Bump `last_flush` on the session for a single instance in a multi-instance map.
pub fn touch_keyed_session(
    sessions: &Mutex<HashMap<String, ActiveSession>>,
    instance_name: &str,
    now: Instant,
) {
    if let Ok(mut guard) = sessions.lock() {
        if let Some(s) = guard.get_mut(instance_name) {
            if s.unpaid_minutes(now) > 0 {
                s.last_flush = now;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_ru_basic() {
        assert_eq!(format_playtime_in(0, PlaytimeLang::Ru), "0 минут");
        assert_eq!(format_playtime_in(1, PlaytimeLang::Ru), "1 минуту");
        assert_eq!(format_playtime_in(2, PlaytimeLang::Ru), "2 минуты");
        assert_eq!(format_playtime_in(5, PlaytimeLang::Ru), "5 минут");
        assert_eq!(format_playtime_in(11, PlaytimeLang::Ru), "11 минут");
        assert_eq!(format_playtime_in(21, PlaytimeLang::Ru), "21 минуту");
        assert_eq!(format_playtime_in(22, PlaytimeLang::Ru), "22 минуты");
        assert_eq!(format_playtime_in(25, PlaytimeLang::Ru), "25 минут");
    }

    #[test]
    fn format_ru_with_hours() {
        assert_eq!(format_playtime_in(60, PlaytimeLang::Ru), "1 час");
        assert_eq!(format_playtime_in(61, PlaytimeLang::Ru), "1 час 1 минуту");
        assert_eq!(format_playtime_in(90, PlaytimeLang::Ru), "1 час 30 минут");
        assert_eq!(format_playtime_in(125, PlaytimeLang::Ru), "2 часа 5 минут");
        assert_eq!(format_playtime_in(3630, PlaytimeLang::Ru), "60 часов 30 минут");
        assert_eq!(format_playtime_in(3661, PlaytimeLang::Ru), "61 час 1 минуту");
    }

    #[test]
    fn format_en_basic() {
        assert_eq!(format_playtime_in(0, PlaytimeLang::En), "0 minutes");
        assert_eq!(format_playtime_in(1, PlaytimeLang::En), "1 minute");
        assert_eq!(format_playtime_in(2, PlaytimeLang::En), "2 minutes");
        assert_eq!(format_playtime_in(5, PlaytimeLang::En), "5 minutes");
        assert_eq!(format_playtime_in(21, PlaytimeLang::En), "21 minutes");
    }

    #[test]
    fn format_en_with_hours() {
        assert_eq!(format_playtime_in(60, PlaytimeLang::En), "1 hour");
        assert_eq!(format_playtime_in(61, PlaytimeLang::En), "1 hour 1 minute");
        assert_eq!(format_playtime_in(90, PlaytimeLang::En), "1 hour 30 minutes");
        assert_eq!(format_playtime_in(125, PlaytimeLang::En), "2 hours 5 minutes");
        assert_eq!(format_playtime_in(3630, PlaytimeLang::En), "60 hours 30 minutes");
        assert_eq!(format_playtime_in(3661, PlaytimeLang::En), "61 hours 1 minute");
    }

    #[test]
    fn format_playtime_wrapper_is_ru() {
        // The legacy `format_playtime` wrapper must keep returning Russian
        // (existing callers + older saves rely on it).
        assert_eq!(format_playtime(60), "1 час");
    }

    fn fake_session(instance_name: &str, started: Instant) -> ActiveSession {
        ActiveSession {
            instance_name: instance_name.to_string(),
            pid: 1234,
            started_at: started,
            last_flush: started,
            child: Arc::new(Mutex::new(None)),
        }
    }

    #[test]
    fn take_keyed_session_returns_and_removes_only_requested() {
        let now = Instant::now();
        let mut map = HashMap::new();
        map.insert("A".to_string(), fake_session("A", now));
        map.insert("B".to_string(), fake_session("B", now));
        let sessions = Mutex::new(map);

        let taken = take_keyed_session(&sessions, "A", now).expect("A session present");
        assert_eq!(taken.0, "A");
        assert_eq!(taken.1, 0);

        // B remains untouched; A is gone.
        {
            let guard = sessions.lock().unwrap();
            assert!(!guard.contains_key("A"));
            assert!(guard.contains_key("B"));
        }
        assert!(take_keyed_session(&sessions, "A", now).is_none());
    }

    #[test]
    fn touch_keyed_session_advances_last_flush_for_one_instance() {
        let start = Instant::now();
        let mut map = HashMap::new();
        map.insert("A".to_string(), fake_session("A", start));
        let sessions = Mutex::new(map);

        let later = start + std::time::Duration::from_secs(121);
        touch_keyed_session(&sessions, "A", later);

        {
            let guard = sessions.lock().unwrap();
            let s = guard.get("A").unwrap();
            assert_eq!(s.unpaid_minutes(later), 0); // advanced flush cursor
        }
    }

    #[test]
    fn flush_all_sessions_writes_only_instances_with_whole_minutes() {
        let start = Instant::now();
        let from = |name: &str| -> ActiveSession {
            // 1 full minute already elapsed for these.
            let mut s = fake_session(name, start - std::time::Duration::from_secs(60));
            s
        };
        let mut map = HashMap::new();
        map.insert("X".to_string(), from("X"));
        map.insert("Y".to_string(), from("Y"));
        let sessions = Mutex::new(map);

        let dir = std::env::temp_dir().join(format!("void_playtime_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let now = Instant::now();
        let flushed = flush_all_sessions(&sessions, &dir, now);
        assert_eq!(flushed, 2);
        assert_eq!(get_playtime(&dir, "X"), 1);
        assert_eq!(get_playtime(&dir, "Y"), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

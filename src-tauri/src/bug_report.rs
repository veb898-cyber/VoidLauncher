//! Safe bug-report collection and redaction.
//!
//! Raw logs are never sent to the renderer. Everything that leaves this
//! module is either generated metadata or text that passed through
//! [`redact_text`]. If a section cannot be redacted safely it is omitted.

use crate::config::AppConfig;
use crate::diagnostics;
use crate::game_logs;
use crate::instances::{self, LoaderType};
use crate::logger;
use crate::AppState;
use serde::Serialize;
use std::path::{Path, PathBuf};
use tauri::State;

const MAX_LOG_LINES: usize = 400;
const MAX_LOG_BYTES: u64 = 128 * 1024;
const MAX_LINE_CHARS: usize = 4000;
const REDACTED: &str = "[REDACTED]";
const INTEGRITY: &str = "[omitted: unsafe data]";

#[derive(Debug, Clone, Serialize)]
pub struct BugReportSection {
    pub id: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BugReportInstance {
    pub name: String,
    pub mc_version: String,
    pub loader: String,
    pub loader_version: Option<String>,
    pub mod_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct BugReportData {
    pub launcher_version: String,
    /// Best-effort OS string from the same checks included in the environment
    /// section. It lets the GitHub issue form prefill a short Windows field
    /// without asking the user to copy it out of the report.
    pub windows_version: Option<String>,
    /// Structured instance metadata for the GitHub issue form. The readable
    /// `instance` section remains the source of truth for the copied report.
    pub instance: Option<BugReportInstance>,
    pub sections: Vec<BugReportSection>,
}

#[derive(Debug, Clone)]
pub struct Redaction {
    pub text: String,
    /// False when a multiline/private-key structure could not be bounded
    /// safely. Log callers must omit the whole section in that case.
    pub safe: bool,
}

fn sensitive_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    let compact: String = lower
        .chars()
        .filter(|c| !matches!(c, '_' | '-' | '.' | ' '))
        .collect();
    lower.contains("token")
        || lower.contains("secret")
        || lower.contains("password")
        || lower.contains("passwd")
        || lower.contains("session")
        || lower.contains("prefetched")
        || lower.contains("authorization")
        || lower.contains("cookie")
        || lower.contains("credential")
        || lower.contains("private_key")
        || compact.contains("apikey")
}

const SECRET_KEYS: &[&str] = &[
    "accessToken",
    "access_token",
    "refreshToken",
    "refresh_token",
    "sessionId",
    "session_id",
    "sessionid",
    "sessionToken",
    "session_token",
    "session",
    "clientSecret",
    "client_secret",
    "apiKey",
    "api_key",
    "api-key",
    "apikey",
    "x-api-key",
    "password",
    "passwd",
    "authorization",
    "cookie",
    "set-cookie",
    "prefetched",
    "authlibinjector.yggdrasil.prefetched",
    "token",
    "secret",
    "credential",
    "private_key",
];

const SECRET_FLAGS: &[&str] = &[
    "accessToken",
    "access_token",
    "refreshToken",
    "refresh_token",
    "sessionId",
    "session_id",
    "sessionToken",
    "session_token",
    "session",
    "clientSecret",
    "apiKey",
    "api_key",
    "api-key",
    "apikey",
    "x-api-key",
    "password",
    "username",
    "uuid",
    "xuid",
    "authlibinjector.yggdrasil.prefetched",
];

fn find_ci(haystack: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    let hay = haystack.as_bytes();
    let needle = needle.as_bytes();
    let first = needle[0].to_ascii_lowercase();
    for index in 0..=hay.len() - needle.len() {
        if hay[index].to_ascii_lowercase() != first {
            continue;
        }
        if hay[index..index + needle.len()]
            .iter()
            .zip(needle)
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
        {
            return Some(index);
        }
    }
    None
}

fn replace_ci(haystack: &str, needle: &str, replacement: &str) -> String {
    if needle.is_empty() {
        return haystack.to_string();
    }
    let mut out = String::with_capacity(haystack.len());
    let mut rest = haystack;
    while let Some(pos) = find_ci(rest, needle) {
        out.push_str(&rest[..pos]);
        out.push_str(replacement);
        rest = &rest[pos + needle.len()..];
    }
    out.push_str(rest);
    out
}

fn is_value_end(byte: u8) -> bool {
    matches!(
        byte,
        b' ' | b'\t' | b',' | b'}' | b']' | b'&' | b';' | b'\r' | b'\n'
    )
}

fn is_json_value_end(byte: u8) -> bool {
    // An unquoted secret can contain spaces (`password: correct horse`).
    // Stop only at a structural delimiter: stopping at the first space would
    // leave the rest of the passphrase in the report. Newlines are still
    // delimiters, and URL query strings stop at `&`/`;`.
    matches!(byte, b',' | b'}' | b']' | b'&' | b';' | b'\r' | b'\n')
}

fn redact_quoted(value: &str, start: usize, quote: u8) -> Option<(usize, String)> {
    let bytes = value.as_bytes();
    let mut index = start + 1;
    while index < bytes.len() {
        if bytes[index] == b'\\' {
            index = index.saturating_add(2);
            continue;
        }
        if bytes[index] == quote {
            let mut replaced = String::new();
            replaced.push_str(&value[..start + 1]);
            replaced.push_str(REDACTED);
            replaced.push_str(&value[index..]);
            // Index just past the closing quote in the *new* string.
            return Some((start + 1 + REDACTED.len() + 1, replaced));
        }
        index += 1;
    }
    None
}

fn redact_authorization(line: &str) -> String {
    let mut out = line.to_string();
    // Header-style secrets run to the end of the line: a Cookie header can
    // contain several `name=value` pairs separated by `;`, and truncating only
    // the first pair would leave the rest exposed.
    for header in ["authorization", "cookie", "set-cookie"] {
        if let Some(pos) = find_ci(&out, header) {
            let after = pos + header.len();
            let bytes = out.as_bytes();
            let mut separator = after;
            while separator < bytes.len() && matches!(bytes[separator], b' ' | b'\t') {
                separator += 1;
            }
            if separator < bytes.len() && matches!(bytes[separator], b':' | b'=') {
                out.truncate(separator + 1);
                out.push(' ');
                out.push_str(REDACTED);
                return out;
            }
        }
    }
    for scheme in ["Bearer", "Basic"] {
        let mut search_from = 0;
        while let Some(relative) = find_ci(&out[search_from..], scheme) {
            let pos = search_from + relative;
            let after = pos + scheme.len();
            let bytes = out.as_bytes();
            let mut value_start = after;
            while value_start < bytes.len() && matches!(bytes[value_start], b' ' | b'\t') {
                value_start += 1;
            }
            if value_start == after || value_start >= bytes.len() {
                search_from = after;
                continue;
            }
            let mut end = value_start;
            while end < bytes.len() && !is_value_end(bytes[end]) && bytes[end] != b'"' {
                end += 1;
            }
            out = format!("{}{}{}", &out[..value_start], REDACTED, &out[end..]);
            search_from = value_start + REDACTED.len();
        }
    }
    out
}

fn leading_indent(line: &str) -> usize {
    line.len() - line.trim_start_matches([' ', '\t']).len()
}

fn looks_like_key_value(trimmed: &str) -> bool {
    let Some((key, _)) = trimmed.split_once([':', '=']) else {
        return false;
    };
    let key = key.trim().trim_matches('"').trim_matches('\'');
    !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ' '))
}

fn looks_like_log_record(trimmed: &str) -> bool {
    trimmed.starts_with('[')
        || trimmed.starts_with("at ")
        || trimmed.starts_with("Caused by:")
        || trimmed.starts_with("... ")
        || trimmed
            .split_whitespace()
            .next()
            .is_some_and(|level| matches!(level, "INFO" | "WARN" | "ERROR" | "DEBUG" | "TRACE"))
}

/// Whether a line after a bare sensitive key is part of its value. This is
/// deliberately conservative: an ambiguous line is masked rather than risking
/// a secret leak.
fn pending_secret_continues(line: &str, pending_indent: usize) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false;
    }
    if leading_indent(line) > pending_indent {
        return true;
    }
    if looks_like_log_record(trimmed) || looks_like_key_value(trimmed) {
        return false;
    }
    true
}

fn redact_key_values(
    line: &str,
    multiline: &mut bool,
    depth: &mut i32,
    pending: &mut Option<usize>,
    indent: usize,
) -> String {
    let mut out = line.to_string();
    for key in SECRET_KEYS {
        let mut search_from = 0;
        loop {
            if search_from >= out.len() {
                break;
            }
            let Some(relative) = find_ci(&out[search_from..], key) else {
                break;
            };
            let pos = search_from + relative;
            let after = pos + key.len();
            let before_ok = pos == 0
                || !out.as_bytes()[pos - 1].is_ascii_alphanumeric()
                    && out.as_bytes()[pos - 1] != b'_';
            let bytes = out.as_bytes();
            let mut separator = after;
            // JSON quotes around the key: `"accessToken" : value`.
            if separator < bytes.len() && bytes[separator] == b'"' {
                separator += 1;
            }
            while separator < bytes.len() && matches!(bytes[separator], b' ' | b'\t') {
                separator += 1;
            }
            if !before_ok || separator >= bytes.len() || !matches!(bytes[separator], b':' | b'=') {
                search_from = after;
                continue;
            }
            let mut value_start = separator + 1;
            while value_start < bytes.len() && matches!(bytes[value_start], b' ' | b'\t') {
                value_start += 1;
            }
            if value_start >= bytes.len() {
                *pending = Some(indent);
                search_from = after;
                continue;
            }
            if bytes[value_start] == b'"' || bytes[value_start] == b'\'' {
                if let Some((next, replaced)) = redact_quoted(&out, value_start, bytes[value_start])
                {
                    out = replaced;
                    search_from = next;
                    continue;
                }
                // An unterminated quoted secret cannot be bounded safely.
                *multiline = true;
                *depth = 1;
                return format!("{}{}", &out[..value_start], REDACTED);
            }
            if matches!(bytes[value_start], b'{' | b'[') {
                let open = bytes[value_start];
                let close = if open == b'{' { b'}' } else { b']' };
                let mut level = 0i32;
                let mut end = value_start;
                let mut found = false;
                while end < bytes.len() {
                    if bytes[end] == open {
                        level += 1;
                    } else if bytes[end] == close {
                        level -= 1;
                        if level == 0 {
                            end += 1;
                            found = true;
                            break;
                        }
                    }
                    end += 1;
                }
                if found {
                    out = format!("{}{}{}", &out[..value_start], REDACTED, &out[end..]);
                    search_from = value_start + REDACTED.len();
                    continue;
                }
                *multiline = true;
                *depth = 1;
                return format!("{}{}", &out[..value_start], REDACTED);
            }
            let mut end = value_start;
            while end < bytes.len() && !is_json_value_end(bytes[end]) {
                end += 1;
            }
            out = format!("{}{}{}", &out[..value_start], REDACTED, &out[end..]);
            search_from = value_start + REDACTED.len();
        }
    }
    out
}

/// Generic `key: value` / `key=value` pass. The fixed [`SECRET_KEYS`] list
/// covers known names; this catches variants such as `my_token`,
/// `id_token` or `secret_key` without needing every spelling.
fn redact_sensitive_pairs(
    line: &str,
    multiline: &mut bool,
    depth: &mut i32,
    pending: &mut Option<usize>,
    indent: usize,
) -> String {
    let mut out = line.to_string();
    let mut search = 0usize;
    loop {
        if search >= out.len() {
            break;
        }
        let bytes = out.as_bytes();
        let Some(relative) = out[search..].find([':', '=']) else {
            break;
        };
        let separator = search + relative;
        // `https://` is not a key/value separator.
        if bytes[separator] == b':'
            && separator + 2 < bytes.len()
            && bytes[separator + 1] == b'/'
            && bytes[separator + 2] == b'/'
        {
            search = separator + 1;
            continue;
        }
        let mut key_start = separator;
        while key_start > 0 {
            let c = bytes[key_start - 1];
            if c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.' | b'"' | b'\'') {
                key_start -= 1;
            } else {
                break;
            }
        }
        let key = out[key_start..separator]
            .trim_matches(|c: char| c == '"' || c == '\'' || c.is_whitespace());
        if !sensitive_key(key) {
            search = separator + 1;
            continue;
        }
        let mut value_start = separator + 1;
        while value_start < bytes.len() && matches!(bytes[value_start], b' ' | b'\t') {
            value_start += 1;
        }
        if value_start >= bytes.len() {
            *pending = Some(indent);
            break;
        }
        if bytes[value_start] == b'"' || bytes[value_start] == b'\'' {
            if let Some((next, replaced)) = redact_quoted(&out, value_start, bytes[value_start]) {
                out = replaced;
                search = next;
                continue;
            }
            *multiline = true;
            *depth = 1;
            return format!("{}{}", &out[..value_start], REDACTED);
        }
        if matches!(bytes[value_start], b'{' | b'[') {
            let open = bytes[value_start];
            let close = if open == b'{' { b'}' } else { b']' };
            let mut level = 0i32;
            let mut end = value_start;
            let mut found = false;
            while end < bytes.len() {
                if bytes[end] == open {
                    level += 1;
                } else if bytes[end] == close {
                    level -= 1;
                    if level == 0 {
                        end += 1;
                        found = true;
                        break;
                    }
                }
                end += 1;
            }
            if found {
                out = format!("{}{}{}", &out[..value_start], REDACTED, &out[end..]);
                search = value_start + REDACTED.len();
                continue;
            }
            *multiline = true;
            *depth = 1;
            return format!("{}{}", &out[..value_start], REDACTED);
        }
        let mut end = value_start;
        while end < bytes.len() && !is_json_value_end(bytes[end]) {
            end += 1;
        }
        out = format!("{}{}{}", &out[..value_start], REDACTED, &out[end..]);
        search = value_start + REDACTED.len();
    }
    out
}

fn redact_jvm_properties(line: &str) -> String {
    let mut out = line.to_string();
    let mut search_from = 0;
    while let Some(relative) = find_ci(&out[search_from..], "-D") {
        let pos = search_from + relative;
        let after = pos + 2;
        let Some(eq_relative) = out[after..].find('=') else {
            break;
        };
        let eq = after + eq_relative;
        let key = &out[after..eq];
        if sensitive_key(key) {
            let mut end = eq + 1;
            let bytes = out.as_bytes();
            while end < bytes.len() && !is_value_end(bytes[end]) {
                end += 1;
            }
            out = format!("{}{}{}", &out[..eq + 1], REDACTED, &out[end..]);
            search_from = eq + 1 + REDACTED.len();
        } else {
            search_from = eq + 1;
        }
    }
    out
}

fn count_braces(line: &str) -> i32 {
    let mut depth = 0;
    for byte in line.bytes() {
        match byte {
            b'{' | b'[' => depth += 1,
            b'}' | b']' => depth -= 1,
            _ => {}
        }
    }
    depth
}

/// Redact secrets and user paths. `safe == false` means the caller must drop
/// the whole section rather than render a partially redacted one.
pub fn redact_text(input: &str, home_dir: Option<&Path>, data_dir: Option<&Path>) -> Redaction {
    let mut out = String::with_capacity(input.len());
    let mut multiline = false;
    let mut depth = 0i32;
    let mut private_key_block = false;
    let mut pending_secret: Option<usize> = None;
    let mut pending_lines = 0usize;
    const MAX_PENDING_LINES: usize = 32;
    let mut safe = true;

    for line in input.lines() {
        let upper = line.to_ascii_uppercase();
        let indent = leading_indent(line);
        let mut line_text = if upper.contains("PRIVATE KEY") {
            pending_secret = None;
            safe = false;
            private_key_block = !upper.contains("END PRIVATE KEY");
            REDACTED.to_string()
        } else if private_key_block {
            if upper.contains("END PRIVATE KEY") {
                private_key_block = false;
            }
            REDACTED.to_string()
        } else if multiline {
            depth += count_braces(line);
            if depth <= 0 {
                multiline = false;
            }
            REDACTED.to_string()
        } else if let Some(pending_indent) = pending_secret {
            if pending_secret_continues(line, pending_indent) {
                pending_lines += 1;
                if pending_lines > MAX_PENDING_LINES {
                    safe = false;
                }
                REDACTED.to_string()
            } else {
                pending_secret = None;
                pending_lines = 0;
                let mut line = redact_jvm_properties(line);
                line = redact_authorization(&line);
                line = redact_key_values(
                    &line,
                    &mut multiline,
                    &mut depth,
                    &mut pending_secret,
                    indent,
                );
                line = redact_sensitive_pairs(
                    &line,
                    &mut multiline,
                    &mut depth,
                    &mut pending_secret,
                    indent,
                );
                line = redact_known_flags(&line);
                line
            }
        } else {
            let mut line = redact_jvm_properties(line);
            line = redact_authorization(&line);
            line = redact_key_values(
                &line,
                &mut multiline,
                &mut depth,
                &mut pending_secret,
                indent,
            );
            line = redact_sensitive_pairs(
                &line,
                &mut multiline,
                &mut depth,
                &mut pending_secret,
                indent,
            );
            line = redact_known_flags(&line);
            line
        };
        if pending_secret.is_some() {
            pending_lines = 0;
        }

        if let Some(home) = home_dir {
            let home = home.to_string_lossy();
            line_text = replace_ci(&line_text, &home, "<HOME>");
            let forward = home.replace('\\', "/");
            if forward != home {
                line_text = replace_ci(&line_text, &forward, "<HOME>");
            }
        }
        if let Some(data) = data_dir {
            let data = data.to_string_lossy();
            line_text = replace_ci(&line_text, &data, "<DATA_DIR>");
            let forward = data.replace('\\', "/");
            if forward != data {
                line_text = replace_ci(&line_text, &forward, "<DATA_DIR>");
            }
        }
        if line_text.chars().count() > MAX_LINE_CHARS {
            line_text = line_text.chars().take(MAX_LINE_CHARS).collect::<String>();
            line_text.push_str(" [truncated]");
        }
        out.push_str(&line_text);
        out.push('\n');
    }
    if multiline || pending_secret.is_some() {
        safe = false;
    }
    Redaction { text: out, safe }
}

fn redact_known_flags(line: &str) -> String {
    let mut out = line.to_string();
    for flag in SECRET_FLAGS {
        for prefix in ["--", "-D"] {
            let marker = format!("{prefix}{flag}");
            let mut search_from = 0;
            while let Some(relative) = find_ci(&out[search_from..], &marker) {
                let pos = search_from + relative;
                let after = pos + marker.len();
                let bytes = out.as_bytes();
                let before_ok =
                    pos == 0 || !bytes[pos - 1].is_ascii_alphanumeric() && bytes[pos - 1] != b'_';
                if !before_ok {
                    search_from = after;
                    continue;
                }
                let mut separator = after;
                if separator < bytes.len() && matches!(bytes[separator], b' ' | b'\t') {
                    separator += 1;
                    while separator < bytes.len() && matches!(bytes[separator], b' ' | b'\t') {
                        separator += 1;
                    }
                } else if separator < bytes.len() && matches!(bytes[separator], b'=' | b':') {
                    separator += 1;
                    while separator < bytes.len() && matches!(bytes[separator], b' ' | b'\t') {
                        separator += 1;
                    }
                } else {
                    search_from = after;
                    continue;
                }
                if separator >= bytes.len() {
                    search_from = after;
                    continue;
                }
                if bytes[separator] == b'"' || bytes[separator] == b'\'' {
                    if let Some((next, replaced)) = redact_quoted(&out, separator, bytes[separator])
                    {
                        out = replaced;
                        search_from = next;
                        continue;
                    }
                }
                let mut end = separator;
                while end < bytes.len() && !is_value_end(bytes[end]) {
                    end += 1;
                }
                out = format!("{}{}{}", &out[..separator], REDACTED, &out[end..]);
                search_from = separator + REDACTED.len();
            }
        }
    }
    out
}

fn read_log_tail(path: &Path) -> std::io::Result<Option<String>> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let len = file.metadata()?.len();
    let start = len.saturating_sub(MAX_LOG_BYTES);
    use std::io::{Read, Seek, SeekFrom};
    let mut file = file;
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::with_capacity((len - start) as usize);
    file.read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    let mut lines: Vec<&str> = text.lines().collect();
    // The byte window may start in the middle of a line (and therefore in
    // the middle of a secret value), so drop that partial first line instead
    // of exposing an unparseable tail of it.
    if start > 0 && !lines.is_empty() {
        lines.remove(0);
    }
    if lines.len() > MAX_LOG_LINES {
        lines = lines.split_off(lines.len() - MAX_LOG_LINES);
    }
    let mut out = String::new();
    if start > 0 {
        out.push_str("[log truncated]\n");
    }
    out.push_str(&lines.join("\n"));
    Ok(Some(out))
}

fn home_dir() -> Option<PathBuf> {
    dirs::home_dir()
}

fn redact_or_omit(text: &str, config: &AppConfig) -> String {
    let redaction = redact_text(text, home_dir().as_deref(), Some(&config.data_dir));
    if redaction.safe {
        redaction.text
    } else {
        INTEGRITY.to_string()
    }
}

fn loader_name(loader: &LoaderType) -> &'static str {
    match loader {
        LoaderType::Vanilla => "Vanilla",
        LoaderType::Fabric => "Fabric",
        LoaderType::Forge => "Forge",
        LoaderType::NeoForge => "NeoForge",
    }
}

/// Build the non-log part of the report. This is intentionally lightweight:
/// it does not run the instance file verification or network probes.
pub fn prepare_report(config: &AppConfig, instance_name: Option<&str>) -> BugReportData {
    let checks = diagnostics::system_checks(config);
    let windows_version = checks
        .iter()
        .find(|check| check.id == "os")
        .map(|check| check.value.trim().to_string())
        .filter(|value| !value.is_empty());
    let mut environment = String::new();
    environment.push_str(&format!("VoidLauncher: {}\n", env!("CARGO_PKG_VERSION")));
    environment.push_str(&format!("OS architecture: {}\n", std::env::consts::ARCH));
    for check in &checks {
        if check.value.trim().is_empty() {
            continue;
        }
        environment.push_str(&format!("{}: {}\n", check.id, check.value.trim()));
    }
    let environment = redact_or_omit(&environment, config);

    let mut sections = vec![BugReportSection {
        id: "environment".to_string(),
        content: environment,
    }];

    let mut report_instance = None;
    if let Some(name) = instance_name {
        if let Ok(instance) = instances::get_instance(&config.instances_dir(), name) {
            let mods_dir = instance.minecraft_dir(&config.instances_dir()).join("mods");
            let mod_count = std::fs::read_dir(&mods_dir)
                .map(|entries| {
                    entries
                        .flatten()
                        .filter(|entry| {
                            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
                            name.ends_with(".jar") || name.ends_with(".jar.disabled")
                        })
                        .count()
                })
                .unwrap_or(0);
            let loader = loader_name(&instance.loader).to_string();
            let mut content = String::new();
            content.push_str(&format!("Instance: {}\n", instance.name));
            content.push_str(&format!("Minecraft: {}\n", instance.mc_version));
            content.push_str(&format!("Loader: {}\n", loader));
            if let Some(version) = &instance.loader_version {
                content.push_str(&format!("Loader version: {}\n", version));
            }
            content.push_str(&format!("Installed mods: {}\n", mod_count));
            sections.push(BugReportSection {
                id: "instance".to_string(),
                content: redact_or_omit(&content, config),
            });
            report_instance = Some(BugReportInstance {
                name: instance.name,
                mc_version: instance.mc_version,
                loader,
                loader_version: instance.loader_version,
                mod_count,
            });
        }
    }

    BugReportData {
        launcher_version: env!("CARGO_PKG_VERSION").to_string(),
        windows_version,
        instance: report_instance,
        sections,
    }
}

pub fn read_log_section(
    config: &AppConfig,
    instance_name: Option<&str>,
    kind: &str,
) -> Result<Option<String>, String> {
    let path = match kind {
        "launcher" => logger::log_path(&config.data_dir),
        "game" => {
            let wanted = instance_name.map(game_logs::sanitize_instance_name);
            let mut sessions = game_logs::list_game_log_sessions(&config.data_dir);
            if let Some(wanted) = wanted {
                sessions.retain(|session| {
                    game_logs::sanitize_instance_name(&session.instance_name) == wanted
                });
            }
            sessions.sort_by_key(|session| {
                std::fs::metadata(&session.path)
                    .and_then(|meta| meta.modified())
                    .ok()
            });
            match sessions.pop() {
                Some(session) => PathBuf::from(&session.path),
                None => return Ok(None),
            }
        }
        _ => return Err(format!("Unknown log kind: {kind}")),
    };
    let Some(raw) =
        read_log_tail(&path).map_err(|_| "Could not read the selected log".to_string())?
    else {
        return Ok(None);
    };
    let redaction = redact_text(&raw, home_dir().as_deref(), Some(&config.data_dir));
    if !redaction.safe {
        return Err("The selected log could not be redacted safely and was omitted".to_string());
    }
    if redaction.text.trim().is_empty() {
        return Ok(None);
    }
    Ok(Some(redaction.text))
}

pub fn crash_section(config: &AppConfig, instance_name: &str) -> Result<Option<String>, String> {
    let analysis = diagnostics::analyze_crash(config, instance_name, None)
        .map_err(|error| error.to_string())?;
    if analysis.source == "none" || analysis.source == "clean" {
        return Ok(None);
    }
    let mut content = String::new();
    content.push_str(&format!("Verdict: {}\n", analysis.verdict));
    content.push_str(&format!("Kind: {}\n", analysis.kind));
    if let Some(culprit) = &analysis.culprit {
        content.push_str(&format!("Culprit: {}\n", culprit));
    }
    content.push_str(&format!("Detail: {}\n", analysis.detail));
    content.push_str(&format!("Source: {}\n", analysis.source));
    let redaction = redact_text(&content, home_dir().as_deref(), Some(&config.data_dir));
    if !redaction.safe {
        return Err("Crash analysis could not be redacted safely and was omitted".to_string());
    }
    Ok(Some(redaction.text))
}

#[tauri::command]
pub async fn cmd_prepare_bug_report(
    state: State<'_, AppState>,
    instance_name: Option<String>,
) -> Result<BugReportData, String> {
    if let Some(name) = &instance_name {
        super::commands::instances::validate_instance_name(name)?;
    }
    let config = {
        let guard = state.config.lock().map_err(|error| error.to_string())?;
        guard.clone()
    };
    tauri::async_runtime::spawn_blocking(move || prepare_report(&config, instance_name.as_deref()))
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn cmd_get_bug_report_log(
    state: State<'_, AppState>,
    instance_name: Option<String>,
    kind: String,
) -> Result<Option<String>, String> {
    if let Some(name) = &instance_name {
        super::commands::instances::validate_instance_name(name)?;
    }
    let config = {
        let guard = state.config.lock().map_err(|error| error.to_string())?;
        guard.clone()
    };
    tauri::async_runtime::spawn_blocking(move || {
        read_log_section(&config, instance_name.as_deref(), &kind)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn cmd_get_bug_report_crash(
    state: State<'_, AppState>,
    instance_name: String,
) -> Result<Option<String>, String> {
    super::commands::instances::validate_instance_name(&instance_name)?;
    let config = {
        let guard = state.config.lock().map_err(|error| error.to_string())?;
        guard.clone()
    };
    tauri::async_runtime::spawn_blocking(move || crash_section(&config, &instance_name))
        .await
        .map_err(|error| error.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn redact(input: &str) -> String {
        let result = redact_text(input, None, None);
        assert!(result.safe, "test input must be safely bounded");
        result.text
    }

    #[test]
    fn removes_access_and_refresh_tokens_from_arguments_and_json() {
        let input = concat!(
            "--accessToken abc.def.ghi --refreshToken=refresh-value\n",
            "{\"accessToken\":\"json-access\",\"refresh_token\":\"json-refresh\"}\n",
            "sessionId=session-secret\n",
            "--session legacy-session-secret\n"
        );
        let output = redact(input);
        assert!(!output.contains("abc.def.ghi"));
        assert!(!output.contains("refresh-value"));
        assert!(!output.contains("json-access"));
        assert!(!output.contains("json-refresh"));
        assert!(!output.contains("session-secret"));
        assert!(!output.contains("legacy-session-secret"));
        assert!(output.contains("accessToken"));
    }

    #[test]
    fn removes_elyby_prefetched_and_authorization_headers() {
        let input = concat!(
            "-Dauthlibinjector.yggdrasil.prefetched=eyJhY2Nlc3NUb2tlbiI6IngifQ==\n",
            "Authorization: Bearer abc123\n",
            "request header authorization=Basic zzz\n",
            "Bearer another-token\n"
        );
        let output = redact(input);
        assert!(!output.contains("eyJhY2Nlc3NUb2tlbiI6IngifQ=="));
        assert!(!output.contains("abc123"));
        assert!(!output.contains("Basic zzz"));
        assert!(!output.contains("another-token"));
    }

    #[test]
    fn redacts_json_fields_case_insensitively() {
        let input = r#"{"AccessToken":"one","CLIENT_SECRET":"two","Cookie":"three","nested":{"password":"four"}}"#;
        let output = redact(input);
        assert!(!output.contains("one"));
        assert!(!output.contains("two"));
        assert!(!output.contains("three"));
        assert!(!output.contains("four"));
    }

    #[test]
    fn masks_home_and_data_paths() {
        let input = r"C:\Users\alice\AppData\Roaming\VoidLauncher\logs\launcher.log";
        let result = redact_text(
            input,
            Some(Path::new(r"C:\Users\alice")),
            Some(Path::new(r"C:\Users\alice\AppData\Roaming\VoidLauncher")),
        );
        assert!(result.safe);
        assert!(!result.text.contains("alice"));
        assert!(result.text.contains("<HOME>"));
    }

    #[test]
    fn redacts_multiple_json_secrets_on_one_line() {
        let input = r#"{"note":"a long non-secret value","accessToken":"first-secret","nested":{"refresh_token":"second-secret"}}"#;
        let output = redact(input);
        assert!(!output.contains("first-secret"));
        assert!(!output.contains("second-secret"));
        assert!(output.contains("a long non-secret value"));
    }

    #[test]
    fn redacts_variant_secret_key_names_and_cookie_headers() {
        let input = concat!(
            "{\"id_token\":\"id-secret\",\"my_secret\":\"custom-secret\"}\n",
            "request id_token=custom-token&page=1\n",
            "Cookie: session=abc; theme=dark\n"
        );
        let output = redact(input);
        assert!(!output.contains("id-secret"));
        assert!(!output.contains("custom-secret"));
        assert!(!output.contains("custom-token"));
        assert!(!output.contains("theme=dark"));
    }

    #[test]
    fn redacts_api_keys_in_json_headers_queries_and_jvm_properties() {
        let input = concat!(
            "{\"apiKey\":\"json-api-key\",\"api_key\":\"json-snake-key\"}\n",
            "X-Api-Key: header-api-key\n",
            "https://example.invalid/?api_key=query-api-key&page=1\n",
            "-Dapi.key=jvm-api-key\n",
            "ordinary_key=keep-this-value\n",
        );
        let output = redact(input);
        for secret in [
            "json-api-key",
            "json-snake-key",
            "header-api-key",
            "query-api-key",
            "jvm-api-key",
        ] {
            assert!(!output.contains(secret), "leaked {secret}: {output}");
        }
        assert!(output.contains("keep-this-value"));
        assert!(output.contains("page=1"));
    }

    #[test]
    fn multiline_secret_is_omitted_as_unsafe() {
        let input = "{\"prefetched\": {\n\"accessToken\": \"secret\"\n";
        let result = redact_text(input, None, None);
        assert!(!result.safe);
        assert!(!result.text.contains("secret"));
    }

    #[test]
    fn private_key_is_never_returned() {
        let input = "-----BEGIN PRIVATE KEY-----\nsecret-material\n-----END PRIVATE KEY-----";
        let result = redact_text(input, None, None);
        assert!(!result.text.contains("secret-material"));
        assert!(!result.safe);
    }

    #[test]
    fn masks_home_paths_with_spaces_and_forward_slashes() {
        let home = Path::new(r"C:\Users\John Doe");
        let input = concat!(
            r"C:\Users\John Doe\AppData\Roaming\VoidLauncher",
            "\n",
            "C:/Users/John Doe/AppData/Local",
        );
        let result = redact_text(input, Some(home), None);
        assert!(result.safe);
        assert!(!result.text.contains("John Doe"));
        assert!(result.text.contains("<HOME>"));
    }

    #[test]
    fn redacts_multiline_secret_continuations_without_eating_following_records() {
        let input = concat!(
            "password:\n",
            "synthetic-password\n",
            "version: 1.2.3\n",
            "secret:\n",
            "  synthetic-secret-line-1\n",
            "  synthetic-secret-line-2\n",
            "api_key:\n",
            "synthetic-api-key\n",
            "\n",
            "[INFO] independent record\n",
        );
        let result = redact_text(input, None, None);
        assert!(result.safe);
        for secret in [
            "synthetic-password",
            "synthetic-secret-line-1",
            "synthetic-secret-line-2",
            "synthetic-api-key",
        ] {
            assert!(
                !result.text.contains(secret),
                "leaked {secret}: {}",
                result.text
            );
        }
        assert!(result.text.contains("version: 1.2.3"));
        assert!(result.text.contains("[INFO] independent record"));
    }

    #[test]
    fn bare_sensitive_key_at_eof_is_fail_closed() {
        let result = redact_text("password:\nsynthetic-eof-secret", None, None);
        assert!(!result.safe);
        assert!(!result.text.contains("synthetic-eof-secret"));
    }

    #[test]
    fn private_key_after_pending_secret_still_starts_the_pem_block() {
        let result = redact_text(
            "password:\n-----BEGIN PRIVATE KEY-----\nsynthetic-pem\n-----END PRIVATE KEY-----",
            None,
            None,
        );
        assert!(!result.safe);
        assert!(!result.text.contains("synthetic-pem"));
    }

    #[test]
    fn line_limit_and_truncation_are_applied() {
        let long = format!("value {}", "x".repeat(MAX_LINE_CHARS + 100));
        let result = redact_text(&long, None, None);
        assert!(result.safe);
        assert!(result.text.contains("[truncated]"));
    }

    #[test]
    fn log_tail_drops_a_partial_first_line() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"--accessToken ");
        bytes.extend(std::iter::repeat_n(b's', 200));
        bytes.push(b'\n');
        bytes.extend(std::iter::repeat_n(b'z', MAX_LOG_BYTES as usize));
        let path = std::env::temp_dir().join(format!(
            "voidlauncher_bug_report_log_{}.txt",
            std::process::id()
        ));
        std::fs::write(&path, &bytes).unwrap();

        let text = read_log_tail(&path).unwrap().unwrap();
        assert!(text.starts_with("[log truncated]"));
        // The window began in the middle of the token value; that partial
        // line must not survive into the report.
        assert!(!text.contains("ssssssss"));

        let _ = std::fs::remove_file(&path);
    }
}

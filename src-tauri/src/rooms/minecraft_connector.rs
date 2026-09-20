//! MinecraftConnector: a thin layer between the room and the game launch.
//!
//! Kept abstract so the join mechanism can evolve (dedicated servers,
//! embedded-server mods) without touching the Room UI: the Room UI sees only
//! "here is the endpoint / join it (or not)".
//!
//! Implementation: detect the "Open to LAN" port from the game's own output
//! stream (captured by `game_logs`), and auto-join version-selectively:
//!   * Minecraft ≤ 1.19.4 → legacy `--server <host> --port <port>`;
//!   * Minecraft 1.20+ (incl. 26.x, snapshots) → Quick Play
//!     `--quickPlayMultiplayer <host>:<port>` plus `--quickPlayPath` so the
//!     client writes its own machine-readable join-log (the version-agnostic
//!     successor of `--server/--port`, removed in snapshot 23w14a).

/// Join mechanism for a MC version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinMechanism {
    /// `--server/--port` (Minecraft ≤ 1.19.4).
    Legacy,
    /// `--quickPlayMultiplayer host:port` (Minecraft 1.20+, 26.x, snapshots).
    QuickPlay,
}

/// Input bytes for port detection (a slice of the live game log / output).
pub trait MinecraftDiscovery: Send + Sync {
    /// Find the advertised "Open to LAN" port in game output. `None` when
    /// the world isn't open yet (or output has nothing LAN-related).
    fn detect_mc_port(&self, log_text: &str) -> Option<u16>;

    /// Which join mechanism applies to `mc_version`, if any.
    fn join_mechanism(&self, mc_version: &str) -> Option<JoinMechanism>;

    /// Build extra game arguments for automatic join. `None` means the given
    /// MC version cannot be auto-jointed by this mechanism (manual join).
    fn build_join_args(&self, mc_version: &str, host: &str, port: u16) -> Option<Vec<String>>;
}

/// MVP discovery: parse the vanilla LAN-server line that Minecraft prints to
/// stdout when "Open to LAN" is activated.
pub struct LanLogDiscovery;

const LAN_PHRASES: &[&str] = &[
    // 1.20.x+ vanilla: "Local game hosted on port 45565"
    "hosted on port",
    // ≤ 1.19 vanilla: "Started LAN server, port 45565"
    "started lan server",
];

impl MinecraftDiscovery for LanLogDiscovery {
    fn detect_mc_port(&self, log_text: &str) -> Option<u16> {
        for line in log_text.lines() {
            let lower = line.to_ascii_lowercase();
            if LAN_PHRASES.iter().any(|p| lower.contains(p)) {
                if let Some(port) = extract_port_after(&lower, "port") {
                    return Some(port);
                }
            }
        }
        None
    }

    fn join_mechanism(&self, mc_version: &str) -> Option<JoinMechanism> {
        if mc_version.trim().is_empty() {
            return None;
        }
        if supports_legacy_server_args(mc_version) {
            Some(JoinMechanism::Legacy)
        } else {
            Some(JoinMechanism::QuickPlay)
        }
    }

    fn build_join_args(&self, mc_version: &str, host: &str, port: u16) -> Option<Vec<String>> {
        match self.join_mechanism(mc_version)? {
            JoinMechanism::Legacy => Some(vec![
                "--server".to_string(),
                host.to_string(),
                "--port".to_string(),
                port.to_string(),
            ]),
            JoinMechanism::QuickPlay => Some(vec![
                "--quickPlayMultiplayer".to_string(),
                compose_endpoint(host, port),
            ]),
        }
    }
}

/// Legacy `--server/--port` args worked until snapshot 23w13a (inclusive),
/// so every released version through 1.19.4 still supports them. They were
/// removed in 23w14a in favour of Quick Play.
fn supports_legacy_server_args(mc_version: &str) -> bool {
    if let Some(rest) = mc_version.strip_prefix("1.") {
        let minor = rest.split('.').next().unwrap_or("");
        if let Ok(n) = minor.parse::<u32>() {
            return n <= 19;
        }
    }
    // Snapshots ("25w05a") and the new yyyy-major scheme ("26.1.2") are modern.
    false
}

/// One entry of the machine-readable Quick Play log the official launcher
/// writes at `--quickPlayPath` when the client successfully joins.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct QuickJoinInfo {
    /// In-game name of the joined server/world (from the server list entry).
    pub name: Option<String>,
}

/// Parse the Quick Play confirmation log: `[{ "type": "multiplayer", "id":
/// "<address>", "name": ..., ... }]`. Returns `Some` when the log records a
/// successful `multiplayer` join — the launcher's proof that the auto-join
/// arguments reached the client and it actually connected.
pub fn parse_quick_play_confirmation(text: &str) -> Option<QuickJoinInfo> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let entries = value.as_array()?;
    for entry in entries {
        if entry.get("type").and_then(|t| t.as_str()) != Some("multiplayer") {
            continue;
        }
        if entry.get("id").and_then(|i| i.as_str()).map(str::is_empty).unwrap_or(true) {
            continue;
        }
        let name = entry.get("name").and_then(|n| n.as_str()).map(str::to_string);
        return Some(QuickJoinInfo { name });
    }
    None
}

/// Scan for the first integer after `port` in a lowercase line, tolerating any
/// punctuation/whitespace between the keyword and the number.
fn extract_port_after(lower_line: &str, needle: &str) -> Option<u16> {
    let idx = lower_line.rfind(needle)? + needle.len();
    let tail = &lower_line[idx..];
    let digits: String = tail
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

/// Compose an endpoint string for the UI, e.g. `host.tail1234.ts.net:45565`
/// or `100.x.y.z:45565`.
pub fn compose_endpoint(host: &str, port: u16) -> String {
    format!("{}:{}", host.trim_end_matches('.'), port)
}

/// High-level facade used by the command layer.
pub struct MinecraftConnector {
    discovery: Box<dyn MinecraftDiscovery>,
}

impl Default for MinecraftConnector {
    fn default() -> Self {
        Self {
            discovery: Box::new(LanLogDiscovery),
        }
    }
}

impl MinecraftConnector {
    pub fn discovery(&self) -> &dyn MinecraftDiscovery {
        self.discovery.as_ref()
    }

    pub fn detect_port(&self, log_text: &str) -> Option<u16> {
        self.discovery.detect_mc_port(log_text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_modern_vanilla_lan_line() {
        let log = "[12:34:56] [Server thread/INFO]: Local game hosted on port 45565";
        assert_eq!(LanLogDiscovery.detect_mc_port(log), Some(45565));
    }

    #[test]
    fn detects_legacy_lan_line() {
        let log = "[09:00:00] [Server thread/INFO]: Started LAN server, port 35577";
        assert_eq!(LanLogDiscovery.detect_mc_port(log), Some(35577));
    }

    #[test]
    fn ignores_output_without_lan_server() {
        let log = "[12:34:56] [Server thread/INFO]: Done (5.2s)! For help, type \"help\"";
        assert_eq!(LanLogDiscovery.detect_mc_port(log), None);
    }

    #[test]
    fn multi_line_log_picks_first_lan_port() {
        let log = "[..] started\n[..] Started LAN server, port 12345\n[..] more\n";
        assert_eq!(LanLogDiscovery.detect_mc_port(log), Some(12345));
    }

    #[test]
    fn legacy_join_args_for_old_versions() {
        // `--server/--port` worked until 23w13a — every release ≤ 1.19.4.
        for ver in ["1.8.9", "1.12.2", "1.16.5", "1.18.2", "1.19.2", "1.19.4"] {
            assert_eq!(
                LanLogDiscovery.join_mechanism(ver),
                Some(JoinMechanism::Legacy),
                "{}",
                ver
            );
            let args = LanLogDiscovery.build_join_args(ver, "100.1.0.1", 12345);
            assert_eq!(
                args,
                Some(vec![
                    "--server".to_string(),
                    "100.1.0.1".to_string(),
                    "--port".to_string(),
                    "12345".to_string(),
                ]),
                "{}",
                ver
            );
        }
    }

    #[test]
    fn quickplay_join_args_for_modern_versions() {
        // Quick Play `--quickPlayMultiplayer host:port` from 23w14a/1.20+,
        // including the 1.21 line, snapshots and the new yyyy-major scheme.
        for ver in ["1.20", "1.20.4", "1.21.4", "26.1.2", "25w05a", "23w14a"] {
            assert_eq!(
                LanLogDiscovery.join_mechanism(ver),
                Some(JoinMechanism::QuickPlay),
                "{}",
                ver
            );
            let args = LanLogDiscovery.build_join_args(ver, "100.1.0.1", 12345);
            assert_eq!(
                args,
                Some(vec![
                    "--quickPlayMultiplayer".to_string(),
                    "100.1.0.1:12345".to_string(),
                ]),
                "{}",
                ver
            );
        }
        // Quick play puts the port inline in the address (unlike legacy).
        let args = LanLogDiscovery.build_join_args("1.21.4", "host.tail.ts.net.", 45565);
        assert_eq!(args, Some(vec![
            "--quickPlayMultiplayer".to_string(),
            "host.tail.ts.net:45565".to_string(),
        ]));
    }

    #[test]
    fn empty_version_has_no_auto_join() {
        assert_eq!(LanLogDiscovery.join_mechanism(""), None);
        assert_eq!(LanLogDiscovery.build_join_args("", "h", 1), None);
    }

    #[test]
    fn parses_quick_play_confirmation_log() {
        // Real-ish payload written by the vanilla client at --quickPlayPath
        // after a successful multiplayer join.
        let log = r#"[
          {
            "type": "multiplayer",
            "id": "host.tail.ts.net:45565",
            "name": "Host's World",
            "lastPlayedTime": "2026-09-20T10:00:00Z",
            "gamemode": "survival"
          }
        ]"#;
        let info = parse_quick_play_confirmation(log);
        assert_eq!(info, Some(QuickJoinInfo { name: Some("Host's World".to_string()) }));

        // A log without a multiplayer entry (e.g. only single-player) is not
        // a join confirmation.
        let sp = r#"[{"type":"singleplayer","id":"World"}]"#;
        assert_eq!(parse_quick_play_confirmation(sp), None);

        assert_eq!(parse_quick_play_confirmation(""), None);
        assert_eq!(parse_quick_play_confirmation("not json"), None);
    }

    #[test]
    fn endpoint_string_uses_dns_and_handles_trailing_dot() {
        assert_eq!(
            compose_endpoint("host.tail1234.ts.net.", 45565),
            "host.tail1234.ts.net:45565"
        );
    }
}
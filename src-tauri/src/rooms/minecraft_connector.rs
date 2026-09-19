//! MinecraftConnector: a thin layer between the room and the game launch.
//!
//! Kept abstract so the join mechanism can evolve (quickPlayMultiplayer,
//! dedicated servers, embedded-server mods) without touching the Room UI:
//! the Room UI sees only "here is the endpoint / join it (or not)".
//!
//! MVP implementation: read the "Open to LAN" port from the game's own
//! output stream (captured by `game_logs`), and join via legacy
//! `--server <host> --port <port>` arguments for Minecraft ≤ 1.18.
//! For newer versions automatic join is not yet wired (needs a
//! compatibility test of `--quickPlayMultiplayer`) — the UI offers the
//! endpoint string for a manual Direct Connect instead.

/// Input bytes for port detection (a slice of the live game log / output).
pub trait MinecraftDiscovery: Send + Sync {
    /// Find the advertised "Open to LAN" port in game output. `None` when
    /// the world isn't open yet (or output has nothing LAN-related).
    fn detect_mc_port(&self, log_text: &str) -> Option<u16>;

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

    fn build_join_args(&self, mc_version: &str, host: &str, port: u16) -> Option<Vec<String>> {
        if !supports_legacy_server_args(mc_version) {
            return None;
        }
        Some(vec![
            "--server".to_string(),
            host.to_string(),
            "--port".to_string(),
            port.to_string(),
        ])
    }
}

/// Legacy `--server/--port` args were dropped when quickPlay landed (1.19+).
fn supports_legacy_server_args(mc_version: &str) -> bool {
    if let Some(rest) = mc_version.strip_prefix("1.") {
        let minor = rest.split('.').next().unwrap_or("");
        if let Ok(n) = minor.parse::<u32>() {
            return n <= 18;
        }
    }
    // Snapshots ("25w05a") and the new yyyy-major scheme ("26.1.2") are modern.
    false
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
        for ver in ["1.8.9", "1.12.2", "1.16.5", "1.18.2"] {
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
    fn modern_versions_have_no_auto_join_yet() {
        for ver in ["1.19.2", "1.20.4", "1.21.4", "26.1.2", "25w05a"] {
            assert_eq!(LanLogDiscovery.build_join_args(ver, "h", 12345), None, "{}", ver);
        }
    }

    #[test]
    fn endpoint_string_uses_dns_and_handles_trailing_dot() {
        assert_eq!(
            compose_endpoint("host.tail1234.ts.net.", 45565),
            "host.tail1234.ts.net:45565"
        );
    }
}
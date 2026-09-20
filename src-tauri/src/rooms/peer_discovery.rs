//! PeerDiscovery: turn `tailscale status --json` into PeerInfo rows and
//! resolve which peer is the room's host machine.
//!
//! Matching strategy (documented in the integration plan):
//!   1. explicit hostname hint — the host optionally stamps its node with
//!      `<room-code>-host` (experimental), giving the guest a deterministic
//!      match;
//!   2. fallback — when the guest has exactly one reachable peer that is not
//!      provably the guest's own node, that peer must be the host (a single
//!      Machine Sharing grant shows up as one peer).

use crate::rooms::tailscale::{PeerNode, TailscaleStatus};
use serde::{Deserialize, Serialize};

/// A peer machine known to the tailnet (excluding the local node).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerInfo {
    pub node_key: String,
    pub host_name: String,
    pub dns_name: String,
    pub tailnet_ips: Vec<String>,
    pub online: bool,
    pub last_seen: Option<String>,
    /// Whether the node was shared with the current user (Machine Sharing).
    /// `Some(false)` marks a node of the guest's OWN account (phone/laptop on
    /// the guest's tailnet) — never a Room host. `None` = unknown/absent.
    pub sharee_node: Option<bool>,
}

/// The resolved host endpoint used for joining and for the VoidLink bridge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostInfo {
    pub node_key: String,
    pub host_name: String,
    pub dns_name: String,
    pub tailnet_ips: Vec<String>,
    pub online: bool,
}

impl From<&PeerNode> for PeerInfo {
    fn from(n: &PeerNode) -> Self {
        Self {
            node_key: n.node_key.clone().unwrap_or_default(),
            host_name: n.host_name.clone().unwrap_or_default(),
            dns_name: n.dns_name.clone().unwrap_or_default(),
            tailnet_ips: n.tailnet_ips.clone().unwrap_or_default(),
            online: n.online.unwrap_or(false),
            last_seen: n.last_seen.clone(),
            sharee_node: n.sharee_node,
        }
    }
}

impl HostInfo {
    pub fn from_peer(p: &PeerInfo) -> Self {
        Self {
            node_key: p.node_key.clone(),
            host_name: p.host_name.clone(),
            dns_name: p.dns_name.clone(),
            tailnet_ips: p.tailnet_ips.clone(),
            online: p.online,
        }
    }

    /// First tailnet IP (join target / VoidLink target).
    pub fn ip(&self) -> Option<&str> {
        self.tailnet_ips.first().map(|s| s.as_str())
    }

    /// MagicDNS address (e.g. `abc-1234-host.tail3e2a10.ts.net`).
    pub fn dns(&self) -> Option<&str> {
        let d = self.dns_name.trim_end_matches('.');
        if d.is_empty() {
            None
        } else {
            Some(d)
        }
    }
}

/// Extract peer rows from a parsed status document. The raw `Peer` field is
/// tolerant of both array and map shapes and of individual malformed entries
/// (see `TailscaleStatus::peers`).
pub fn peers_from_status(status: &TailscaleStatus) -> Vec<PeerInfo> {
    status.peers().iter().map(PeerInfo::from).collect()
}

/// Resolve the host peer for a room.
///
/// * `room_id` — the UI room code (`ABCD-1234`) used for the hostname hint.
///
/// Returns `None` when the host cannot be identified yet (offline / share not
/// accepted / room code wrong).
pub fn resolve_host_for_room(peers: &[PeerInfo], room_id: Option<&str>) -> Option<HostInfo> {
    let hint = room_id.and_then(|id| crate::rooms::tailscale::sanitize_hostname(id).ok());
    let hint_prefix = hint.map(|h| format!("{}-host", h));

    // 1. Explicit hostname hint match.
    if let Some(prefix) = &hint_prefix {
        if let Some(peer) = peers
            .iter()
            .find(|p| p.dns_name.to_ascii_lowercase().starts_with(prefix) || p.host_name.to_ascii_lowercase().starts_with(prefix))
        {
            return Some(HostInfo::from_peer(peer));
        }
    }

    // 2. Fallback: a single reachable peer that is not provably the guest's
    //    own node is the shared host machine. `ShareeNode: false` marks a
    //    node of the current account (own phone/laptop on the guest's
    //    tailnet) — taking it for the host would misdirect the join, so it is
    //    excluded. Peers whose flag is true or unknown stay eligible so the
    //    default Machine Sharing flow keeps working.
    let online: Vec<&PeerInfo> = peers
        .iter()
        .filter(|p| p.online && !p.tailnet_ips.is_empty())
        .filter(|p| p.sharee_node != Some(false))
        .collect();
    if online.len() == 1 {
        return Some(HostInfo::from_peer(online[0]));
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rooms::tailscale::{PeerNode, TailscaleStatus};

    /// A peer flagged `ShareeNode: false` — a node of the current account
    /// (the guest's own device), or a generic fixture.
    fn node(key: &str, host: &str, dns: &str, ip: Option<&str>, online: bool) -> PeerNode {
        PeerNode {
            node_key: Some(key.into()),
            host_name: Some(host.into()),
            dns_name: Some(dns.into()),
            tailnet_ips: ip.map(|i| vec![i.into()]),
            online: Some(online),
            last_seen: if online { Some("now".into()) } else { None },
            sharee_node: Some(false),
            logged_in: None,
            user_id: None,
        }
    }
    fn shared_node(key: &str, host: &str, dns: &str, ip: Option<&str>, online: bool) -> PeerNode {
        PeerNode {
            sharee_node: Some(true),
            ..node(key, host, dns, ip, online)
        }
    }

    /// A peer whose `NodeKey` is absent in the raw status document (seen in
    /// real-world output) — must be kept as a host candidate, not dropped.
    fn keyless_node(host: &str, dns: &str, ip: Option<&str>, online: bool) -> PeerNode {
        PeerNode {
            node_key: None,
            host_name: Some(host.into()),
            dns_name: Some(dns.into()),
            tailnet_ips: ip.map(|i| vec![i.into()]),
            online: Some(online),
            last_seen: Some("now".into()),
            sharee_node: Some(true),
            logged_in: None,
            user_id: None,
        }
    }

    fn status_with(peers: Vec<PeerNode>) -> TailscaleStatus {
        TailscaleStatus {
            version: None,
            backend_state: Some("Running".into()),
            auth_url: None,
            current_tailnet: None,
            self_node: None,
            peer: Some(serde_json::to_value(peers).unwrap()),
            user: None,
            magic_dns_suffix: None,
        }
    }

    #[test]
    fn hostname_hint_wins_even_with_other_online_peers() {
        let peers = vec![
            node("android", "android-phone", "android-phone.tail1234.ts.net.", Some("100.1.0.2"), true),
            shared_node("host", "abcd-1234-host", "abcd-1234-host.tail1234.ts.net.", Some("100.1.0.1"), true),
        ];
        let peers = peers_from_status(&status_with(peers));
        let host = resolve_host_for_room(&peers, Some("ABCD-1234")).unwrap();
        assert_eq!(host.host_name, "abcd-1234-host");
        assert_eq!(host.ip(), Some("100.1.0.1"));
    }

    #[test]
    fn single_online_peer_fallback() {
        let peers = vec![
            node("android", "android-phone", "android.tail1234.ts.net.", None, false),
            shared_node("host", "gaming-pc", "gaming-pc.tail1234.ts.net.", Some("100.1.0.9"), true),
        ];
        let peers = peers_from_status(&status_with(peers));
        let host = resolve_host_for_room(&peers, None).unwrap();
        assert_eq!(host.node_key, "host");
        assert_eq!(host.dns(), Some("gaming-pc.tail1234.ts.net"));
    }

    #[test]
    fn own_online_device_is_never_taken_for_the_host() {
        // The guest's own phone (`ShareeNode: false`) is the only online peer
        // — it is the guest, NOT the shared host, and must never be resolved
        // as the host.
        let peers = vec![node(
            "phone",
            "my-phone",
            "my-phone.tail1234.ts.net.",
            Some("100.1.0.7"),
            true,
        )];
        let peers = peers_from_status(&status_with(peers));
        assert!(resolve_host_for_room(&peers, None).is_none());
    }

    #[test]
    fn unknown_share_flag_peer_still_qualifies_for_fallback() {
        // Backward compatibility: when the status document omits `ShareeNode`,
        // the fallback keeps its historical behavior (single online peer is
        // taken as the host).
        let p = PeerNode {
            sharee_node: None,
            ..node("host", "gaming-pc", "gaming-pc.tail1234.ts.net.", Some("100.1.0.9"), true)
        };
        let peers = peers_from_status(&status_with(vec![p]));
        let host = resolve_host_for_room(&peers, None).unwrap();
        assert_eq!(host.node_key, "host");
    }

    #[test]
    fn node_without_key_is_still_a_host_candidate() {
        // Regression: a real status document can contain a peer without
        // `NodeKey`; it must remain eligible for host resolution (key is an
        // identifier, the DNS name / IP carry the connectivity data).
        let peers = vec![keyless_node("gaming-pc", "abcd-1234-host.tail1234.ts.net.", Some("100.1.0.9"), true)];
        let peers = peers_from_status(&status_with(peers));
        let host = resolve_host_for_room(&peers, Some("ABCD-1234")).unwrap();
        assert_eq!(host.host_name, "gaming-pc");
        assert_eq!(host.ip(), Some("100.1.0.9"));
    }

    #[test]
    fn ambiguous_or_missing_host_yields_none() {
        let two_online = vec![
            node("a", "a", "a.tail.ts.net.", Some("100.1.0.1"), true),
            node("b", "b", "b.tail.ts.net.", Some("100.1.0.2"), true),
        ];
        let peers = peers_from_status(&status_with(two_online));
        assert!(resolve_host_for_room(&peers, None).is_none());

        let all_offline = vec![node("a", "a", "a.tail.ts.net.", Some("100.1.0.1"), false)];
        let peers = peers_from_status(&status_with(all_offline));
        assert!(resolve_host_for_room(&peers, Some("ABCD-1234")).is_none());
    }

    #[test]
    fn hint_is_case_insensitive_and_works_without_room_id() {
        let peers = vec![node("h", "Hz-Pc", "AB12-CD34-HOST.tail.ts.net.", Some("100.1.0.5"), false)];
        let peers = peers_from_status(&status_with(peers));
        // A room id that doesn't match the hint and no single online peer → none.
        assert!(resolve_host_for_room(&peers, Some("ZZZZ-9999")).is_none());
        // Matches case-insensitively on the DNS name.
        let host = resolve_host_for_room(&peers, Some("ab12-cd34")).unwrap();
        assert_eq!(host.host_name, "Hz-Pc");
    }

    #[test]
    fn host_info_dns_trims_trailing_dot() {
        let peers = vec![shared_node("h", "h", "h.tail1234.ts.net.", Some("100.1.0.5"), true)];
        let peers = peers_from_status(&status_with(peers));
        let host = resolve_host_for_room(&peers, None).unwrap();
        assert_eq!(host.dns(), Some("h.tail1234.ts.net"));
    }
}
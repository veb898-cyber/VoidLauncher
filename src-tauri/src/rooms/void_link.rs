//! VoidLink: a small control-plane bridge between two VoidLauncher instances
//! over the Tailscale net.
//!
//! * Host side runs a tiny HTTP listener bound to the host's tailnet IP and
//!   answers `GET /room/status?code=<RoomID>` with the current Minecraft LAN
//!   port. Minecraft gameplay traffic never flows through this bridge — only
//!   the tiny control payload does.
//! * Guest side polls that endpoint; as soon as `mc_port` is non-null the
//!   Join World button becomes live.
//!
//! The bridge is authorized by the room code (an app-level value, NOT a
//! Tailscale secret). Binding only to the tailnet IP keeps it unreachable
//! from the LAN/Wi-Fi (Tailscale's own firewall profile permits inbound to
//! any process — verified during live testing).
//!
//! The server is deliberately a minimal hand-rolled HTTP/1.1 responder on
//! `tokio::TcpListener`: the protocol surface is one GET route, so pulling in
//! a full web framework would be unjustified weight. The guest client uses the
//! existing `reqwest` stack.

use crate::rooms::room_state::{RoomRole, RoomStateManager};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Default TCP port of the control bridge.
pub const VOID_LINK_PORT: u16 = 48888;

/// The currently bound host-side bridge task, if any. Only one bridge ever
/// lives at a time. The task is tied to the tailnet IP it was bound to; when
/// the room ends or the IP changes, the old task is aborted and a fresh
/// bridge is started on the current address.
static VOID_LINK_STATE: std::sync::OnceLock<std::sync::Mutex<Option<BridgeTask>>> =
    std::sync::OnceLock::new();

/// A running control-bridge task bound to a single tailnet IP.
struct BridgeTask {
    /// Tailnet IP this bridge is bound to (e.g. `100.101.102.103`).
    ip: String,
    task: tokio::task::JoinHandle<()>,
}

fn void_link_state() -> &'static std::sync::Mutex<Option<BridgeTask>> {
    VOID_LINK_STATE.get_or_init(|| std::sync::Mutex::new(None))
}

/// Control payload the host's bridge answers with.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VoidLinkStatus {
    pub room_id: String,
    pub mc_port: Option<u16>,
}

/// Room-code check: the query parameter must match the room the host serves
/// (case-insensitive). An empty/missing code is always rejected.
pub fn authorize(code: Option<&str>, expected_room: Option<&str>) -> bool {
    let Some(code) = code.filter(|c| !c.trim().is_empty()) else {
        return false;
    };
    match expected_room {
        Some(room) => room.eq_ignore_ascii_case(code.trim()),
        None => false,
    }
}

/// Run the host-side bridge on `listener`. Accepts one connection at a time
/// (concurrently spawned), answers a single `GET /room/status`, then closes.
pub async fn void_link_server(
    listener: tokio::net::TcpListener,
    room_state: Arc<RoomStateManager>,
) -> std::io::Result<()> {
    loop {
        let (socket, _peer) = listener.accept().await?;
        let room = room_state.clone();
        tokio::spawn(async move {
            if let Err(e) = serve_connection(socket, room).await {
                tracing::debug!(target: "rooms", "VoidLink connection error: {}", e);
            }
        });
    }
}

/// Read one HTTP request head, dispatch `/room/status`, and write the reply.
async fn serve_connection(
    mut socket: tokio::net::TcpStream,
    room: Arc<RoomStateManager>,
) -> std::io::Result<()> {
    let mut buf = [0u8; 2048];
    let mut filled = 0usize;
    let header_len = loop {
        let n = socket.read(&mut buf[filled..]).await?;
        if n == 0 {
            return Ok(());
        }
        filled += n;
        if let Some(pos) = find_subslice(&buf[..filled], b"\r\n\r\n") {
            break pos + 4;
        }
        if filled == buf.len() {
            return write_response(&mut socket, 431, "request too large").await;
        }
    };

    let head = String::from_utf8_lossy(&buf[..header_len]);
    let request_line = head.lines().next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("");

    if method != "GET" {
        return write_response(&mut socket, 405, "method not allowed").await;
    }
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if path != "/room/status" {
        return write_response(&mut socket, 404, "not found").await;
    }

    let code = query_param(query, "code");
    let snapshot = room.snapshot();
    if snapshot.role != RoomRole::Host || !authorize(code.as_deref(), snapshot.room_id.as_deref()) {
        return write_response(&mut socket, 403, "invalid room code").await;
    }

    let body = serde_json::to_string(&VoidLinkStatus {
        room_id: snapshot.room_id.unwrap_or_default(),
        mc_port: snapshot.minecraft_port,
    })
    .unwrap_or_else(|_| "{}".to_string());
    write_response(&mut socket, 200, &body).await
}

/// Write a complete HTTP/1.1 response with `Connection: close`, then flush.
async fn write_response(
    socket: &mut tokio::net::TcpStream,
    status: u16,
    body: &str,
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        431 => "Request Header Fields Too Large",
        _ => "Error",
    };
    let content_type = if status == 200 {
        "application/json"
    } else {
        "text/plain; charset=utf-8"
    };
    let response = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        status,
        reason,
        content_type,
        body.len(),
        body
    );
    socket.write_all(response.as_bytes()).await?;
    socket.flush().await
}

/// Guest-side client: query the host bridge, ignoring proxy settings (this
/// is a direct tailnet peer). Returns the parsed control status.
pub async fn query_host_status(
    host_ip: &str,
    port: u16,
    room_code: &str,
) -> crate::error::Result<VoidLinkStatus> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| crate::error::LauncherError::Network(e))?;
    let url = format!("http://{host_ip}:{port}/room/status?code={room_code}");
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| crate::error::LauncherError::Network(e))?;
    if resp.status() != reqwest::StatusCode::OK {
        return Err(crate::error::LauncherError::Download(format!(
            "VoidLink: host returned {}",
            resp.status()
        )));
    }
    resp.json::<VoidLinkStatus>()
        .await
        .map_err(crate::error::LauncherError::Network)
}

/// Resolve the address the host's bridge should bind to: `self_ip:VOID_LINK_PORT`.
pub fn bind_addr(self_ip: &str, port: u16) -> Option<SocketAddr> {
    let ip: std::net::IpAddr = self_ip.parse().ok()?;
    Some(SocketAddr::new(ip, port))
}

/// Start the host-side bridge on the tailnet IP. Safe to call repeatedly:
/// a bridge already bound to the *same* IP is reused (idempotent no-op);
/// a bridge bound to a different/stale IP (or an exited accept loop) is
/// aborted and a fresh listener is bound to the requested address.
pub async fn ensure_void_link_server(
    self_ip: String,
    room_state: Arc<RoomStateManager>,
) -> crate::error::Result<()> {
    // Reconcile the current bridge OUTSIDE the await: no lock may be held
    // across `bind().await` (the Tauri command future must stay `Send`).
    {
        let mut state = void_link_state()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        match state.as_mut() {
            Some(b) if b.ip == self_ip && !b.task.is_finished() => {
                return Ok(());
            }
            Some(b) => {
                // Stale bridge: bound to a previous IP, or the accept loop
                // exited. Abort it so the old listener stops serving, then
                // re-bind below.
                if !b.task.is_finished() {
                    b.task.abort();
                }
            }
            None => {}
        }
        *state = None;
    } // guard dropped here, before the await

    let port = room_state.void_link_port();
    let addr = bind_addr(&self_ip, port).ok_or_else(|| {
        crate::error::LauncherError::Launch(format!(
            "Invalid tailnet IP for the bridge: {}",
            self_ip
        ))
    })?;
    let listener = tokio::net::TcpListener::bind(addr).await.map_err(|e| {
        crate::error::LauncherError::Launch(format!("VoidLink bind {}: {}", addr, e))
    })?;
    tracing::info!(target: "rooms", "VoidLink bridge listening on {}", addr);
    let task = tokio::spawn(async move {
        if let Err(e) = void_link_server(listener, room_state).await {
            tracing::error!(target: "rooms", "VoidLink server error: {}", e);
        }
    });

    // Own the bridge now that it is fully started.
    let mut state = void_link_state()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    *state = Some(BridgeTask { ip: self_ip, task });
    Ok(())
}

/// Abort the host-side bridge (the room ended). The bridge never outlives a
/// room session; the next `ensure_void_link_server` binds a fresh listener on
/// the current tailnet IP.
pub fn stop_void_link_server() {
    let mut state = void_link_state()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(b) = state.take() {
        if !b.task.is_finished() {
            b.task.abort();
        }
    }
}

/// Locate `needle` in `haystack`; used to find the end of HTTP headers.
fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Extract a raw (not yet decoded) query parameter value.
fn query_param(query: &str, key: &str) -> Option<String> {
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        if k == key {
            return Some(v.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorize_matches_room_case_insensitively() {
        assert!(authorize(Some("ABCD-1234"), Some("abcd-1234")));
        assert!(authorize(Some(" abcd-1234 "), Some("ABCD-1234")));
        assert!(!authorize(Some("ABCD-1234"), Some("WXYZ-9012")));
        assert!(!authorize(None, Some("ABCD-1234")));
        assert!(!authorize(Some(""), Some("ABCD-1234")));
        assert!(!authorize(Some("ABCD-1234"), None));
    }

    #[test]
    fn bind_addr_parses_tailnet_ip() {
        let addr = bind_addr("100.101.102.103", VOID_LINK_PORT).unwrap();
        assert_eq!(addr.to_string(), "100.101.102.103:48888");
        assert!(bind_addr("not-an-ip", VOID_LINK_PORT).is_none());
    }

    #[test]
    fn query_param_extracts_the_code() {
        assert_eq!(query_param("code=ABCD-1234", "code").as_deref(), Some("ABCD-1234"));
        assert_eq!(query_param("x=1&code=ABCD-1234&y=2", "code").as_deref(), Some("ABCD-1234"));
        assert_eq!(query_param("x=1", "code"), None);
        assert_eq!(query_param("", "code"), None);
    }

    #[test]
    fn finds_header_terminator_across_buffer() {
        let data = b"GET /room/status?code=ABCD-1234 HTTP/1.1\r\nHost: x\r\n\r\n";
        assert_eq!(find_subslice(data, b"\r\n\r\n"), Some(data.len() - 4));
        assert_eq!(find_subslice(b"GET / HTTP/1.1\r\n", b"\r\n\r\n"), None);
    }
}

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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Default TCP port of the control bridge.
pub const VOID_LINK_PORT: u16 = 48888;

/// Single-instance guard so only one bridge task ever binds the port.
static VOID_LINK_TASK_RUNNING: AtomicBool = AtomicBool::new(false);

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

/// Start the host-side bridge on the tailnet IP. Safe to call repeatedly —
/// only the first call actually binds the port; later calls are no-ops.
pub async fn ensure_void_link_server(
    self_ip: String,
    room_state: Arc<RoomStateManager>,
) -> crate::error::Result<()> {
    if VOID_LINK_TASK_RUNNING.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
    let port = room_state.void_link_port();
    let bound = async {
        let addr = bind_addr(&self_ip, port).ok_or_else(|| {
            crate::error::LauncherError::Launch(format!("Invalid tailnet IP for the bridge: {}", self_ip))
        })?;
        let listener = tokio::net::TcpListener::bind(addr).await.map_err(|e| {
            crate::error::LauncherError::Launch(format!("VoidLink bind {}: {}", addr, e))
        })?;
        Ok::<_, crate::error::LauncherError>((addr, listener))
    }
    .await;

    let (addr, listener) = match bound {
        Ok(pair) => pair,
        Err(e) => {
            // Release the single-instance guard: a failed start must not make
            // every later attempt a silent no-op ("Ok" without a bridge).
            VOID_LINK_TASK_RUNNING.store(false, Ordering::SeqCst);
            return Err(e);
        }
    };
    tracing::info!(target: "rooms", "VoidLink bridge listening on {}", addr);
    tokio::spawn(async move {
        if let Err(e) = void_link_server(listener, room_state).await {
            tracing::error!(target: "rooms", "VoidLink server error: {}", e);
        }
        VOID_LINK_TASK_RUNNING.store(false, Ordering::SeqCst);
    });
    Ok(())
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

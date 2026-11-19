//! VoidLink: a small control-plane bridge between two VoidLauncher instances
//! over the Tailscale net.
//!
//! * Host side runs a tiny HTTP listener bound to the host's tailnet IP and
//!   answers `GET /room/status?room=<RoomID>&challenge=<nonce>&auth=<proof>`
//!   with the current Minecraft LAN port and a host proof. Minecraft gameplay
//!   traffic never flows through this bridge — only the tiny control payload
//!   does.
//! * Guest side polls that endpoint; as soon as `mc_port` is non-null the
//!   Join World button becomes live.
//!
//! The bridge is authenticated by a per-room invitation secret with a fresh
//! challenge-response (HMAC-SHA256). The secret is never sent over the wire:
//! the guest proves knowledge with `auth`, the host proves knowledge with
//! `proof` bound to its own address, the challenge and the advertised port.
//! Binding only to the tailnet IP keeps the bridge unreachable from the
//! LAN/Wi-Fi (Tailscale's own firewall profile permits inbound to any process
//! — verified during live testing).
//!
//! The server is deliberately a minimal hand-rolled HTTP/1.1 responder on
//! `tokio::TcpListener`: the protocol surface is one GET route, so pulling in
//! a full web framework would be unjustified weight. The guest client uses the
//! existing `reqwest` stack.

use crate::rooms::room_state::{RoomRole, RoomStateManager};
use futures::StreamExt;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Default TCP port of the control bridge.
pub const VOID_LINK_PORT: u16 = 48888;

/// A tailnet peer may legitimately have several launchers, but no more than a
/// few simultaneous control requests. Excess connections are dropped instead
/// of accumulating unbounded tasks.
const MAX_CONNECTIONS: usize = 16;
/// A request that does not deliver a complete header in this window is
/// abandoned; otherwise an idle/malicious peer could pin a task forever.
#[cfg(not(test))]
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
#[cfg(test)]
const REQUEST_TIMEOUT: Duration = Duration::from_millis(100);
/// Maximum accepted control response body. The real payload is tiny; this
/// prevents a malicious peer from making the guest allocate an arbitrary
/// JSON document.
const MAX_RESPONSE_BYTES: usize = 4096;
/// Maximum request header accepted by the bridge.
const MAX_REQUEST_BYTES: usize = 2048;
/// Challenge format: 32 lowercase hex characters (122 bits from UUID v4).
const CHALLENGE_HEX_LEN: usize = 32;
/// HMAC-SHA256 proof format: 64 lowercase hex characters.
const PROOF_HEX_LEN: usize = 64;

/// Admission control for the bridge. The same helper is used by the live
/// server and by the deterministic tests, so the limit tested is the limit
/// production enforces.
#[derive(Clone)]
struct ConnectionLimit {
    slots: Arc<tokio::sync::Semaphore>,
}

impl ConnectionLimit {
    fn new(max_connections: usize) -> Self {
        Self {
            slots: Arc::new(tokio::sync::Semaphore::new(max_connections)),
        }
    }

    fn try_admit(&self) -> Option<tokio::sync::OwnedSemaphorePermit> {
        self.slots.clone().try_acquire_owned().ok()
    }

    #[cfg(test)]
    fn available(&self) -> usize {
        self.slots.available_permits()
    }
}

/// The currently bound host-side bridge task, if any. Only one bridge ever
/// lives at a time. The task is tied to the tailnet IP it was bound to; when
/// the room ends or the IP changes, the old task is aborted and a fresh
/// bridge is started on the current address.
static VOID_LINK_STATE: std::sync::OnceLock<std::sync::Mutex<Option<BridgeTask>>> =
    std::sync::OnceLock::new();

/// Serializes start/stop transitions. The bridge state itself only needs a
/// short std-mutex critical section, but `ensure` has to release it while
/// `TcpListener::bind().await` runs; without this operation lock two starts
/// could each bind and one task would be orphaned, and a stop racing a start
/// could miss the task that is installed after the stop.
static VOID_LINK_OP_LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();

fn void_link_op_lock() -> &'static tokio::sync::Mutex<()> {
    VOID_LINK_OP_LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// A running control-bridge task bound to a single tailnet IP.
struct BridgeTask {
    /// Tailnet IP this bridge is bound to (e.g. `100.101.102.103`).
    ip: String,
    task: tokio::task::JoinHandle<()>,
}

fn void_link_state() -> &'static std::sync::Mutex<Option<BridgeTask>> {
    VOID_LINK_STATE.get_or_init(|| std::sync::Mutex::new(None))
}

/// Control payload the host's bridge answers with. The host proves knowledge
/// of the invitation secret over `host_ip`, `challenge` and `mc_port`; a peer
/// that only overheard the public room id cannot produce a valid `proof`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VoidLinkStatus {
    pub room_id: String,
    pub mc_port: Option<u16>,
    pub host_ip: String,
    pub challenge: String,
    pub proof: String,
}

type HmacSha256 = Hmac<Sha256>;

fn canonical_room_id(room_id: &str) -> String {
    room_id.trim().to_ascii_uppercase()
}

fn canonical_ip(ip: &str) -> Option<String> {
    ip.trim()
        .parse::<std::net::IpAddr>()
        .ok()
        .map(|ip| ip.to_string())
}

fn is_lower_hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn generate_challenge() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

fn guest_auth_message(room_id: &str, challenge: &str) -> String {
    format!(
        "voidlink-guest-v1\n{}\n{}",
        canonical_room_id(room_id),
        challenge.to_ascii_lowercase()
    )
}

fn host_proof_message(
    room_id: &str,
    host_ip: &str,
    challenge: &str,
    mc_port: Option<u16>,
) -> String {
    let port = mc_port
        .map(|port| port.to_string())
        .unwrap_or_else(|| "-".to_string());
    format!(
        "voidlink-host-v1\n{}\n{}\n{}\n{}",
        canonical_room_id(room_id),
        canonical_ip(host_ip).unwrap_or_else(|| host_ip.trim().to_string()),
        challenge.to_ascii_lowercase(),
        port
    )
}

fn hmac_hex(secret_hex: &str, message: &str) -> Option<String> {
    let key = hex::decode(secret_hex).ok()?;
    if key.len() != 32 {
        return None;
    }
    let mut mac = HmacSha256::new_from_slice(&key).ok()?;
    mac.update(message.as_bytes());
    Some(hex::encode(mac.finalize().into_bytes()))
}

fn verify_hmac_hex(secret_hex: &str, message: &str, proof_hex: &str) -> bool {
    if !is_lower_hex(proof_hex, PROOF_HEX_LEN) {
        return false;
    }
    let Ok(key) = hex::decode(secret_hex) else {
        return false;
    };
    if key.len() != 32 {
        return false;
    }
    let Ok(proof) = hex::decode(proof_hex) else {
        return false;
    };
    let Ok(mut mac) = HmacSha256::new_from_slice(&key) else {
        return false;
    };
    mac.update(message.as_bytes());
    mac.verify_slice(&proof).is_ok()
}

/// Guest proof that it knows the invitation secret. The secret itself is
/// never sent: only an HMAC over this invitation's fresh challenge.
fn guest_auth(secret_hex: &str, room_id: &str, challenge: &str) -> Option<String> {
    hmac_hex(secret_hex, &guest_auth_message(room_id, challenge))
}

fn verify_guest_auth(
    secret_hex: &str,
    room_id: &str,
    challenge: &str,
    auth_hex: Option<&str>,
) -> bool {
    let Some(auth_hex) = auth_hex else {
        return false;
    };
    verify_hmac_hex(
        secret_hex,
        &guest_auth_message(room_id, challenge),
        auth_hex,
    )
}

fn host_proof(
    secret_hex: &str,
    room_id: &str,
    host_ip: &str,
    challenge: &str,
    mc_port: Option<u16>,
) -> Option<String> {
    hmac_hex(
        secret_hex,
        &host_proof_message(room_id, host_ip, challenge, mc_port),
    )
}

fn verify_host_proof(
    secret_hex: &str,
    room_id: &str,
    host_ip: &str,
    challenge: &str,
    mc_port: Option<u16>,
    proof_hex: &str,
) -> bool {
    verify_hmac_hex(
        secret_hex,
        &host_proof_message(room_id, host_ip, challenge, mc_port),
        proof_hex,
    )
}

/// Public room-id check. This is not the authorization anymore: it only
/// selects which room a candidate claims to serve. The challenge-response
/// below is what proves the claim.
pub fn authorize_room(room: Option<&str>, expected_room: Option<&str>) -> bool {
    let Some(room) = room.map(str::trim).filter(|room| !room.is_empty()) else {
        return false;
    };
    match expected_room {
        Some(expected) => expected.eq_ignore_ascii_case(room),
        None => false,
    }
}

/// Run the host-side bridge on `listener`. Connections are handled
/// concurrently up to [`MAX_CONNECTIONS`]; each answers a single
/// `GET /room/status`, then closes.
pub async fn void_link_server(
    listener: tokio::net::TcpListener,
    room_state: Arc<RoomStateManager>,
) -> std::io::Result<()> {
    let limit = ConnectionLimit::new(MAX_CONNECTIONS);
    loop {
        let (socket, _peer) = listener.accept().await?;
        let Some(permit) = limit.try_admit() else {
            tracing::warn!(target: "rooms", "VoidLink connection limit reached; dropping request");
            drop(socket);
            continue;
        };
        let room = room_state.clone();
        tokio::spawn(async move {
            let _permit = permit;
            match tokio::time::timeout(REQUEST_TIMEOUT, serve_connection(socket, room)).await {
                Ok(Err(e)) => tracing::debug!(target: "rooms", "VoidLink connection error: {}", e),
                Err(_) => tracing::debug!(target: "rooms", "VoidLink request timed out"),
                Ok(Ok(())) => {}
            }
        });
    }
}

/// Read one HTTP request head, dispatch `/room/status`, and write the reply.
async fn serve_connection(
    mut socket: tokio::net::TcpStream,
    room: Arc<RoomStateManager>,
) -> std::io::Result<()> {
    let mut buf = [0u8; MAX_REQUEST_BYTES];
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

    let room_query = query_param(query, "room");
    let challenge = query_param(query, "challenge").map(|value| value.trim().to_ascii_lowercase());
    let auth = query_param(query, "auth").map(|value| value.trim().to_ascii_lowercase());
    let snapshot = room.snapshot();
    if snapshot.role != RoomRole::Host
        || !authorize_room(room_query.as_deref(), snapshot.room_id.as_deref())
    {
        return write_response(&mut socket, 403, "invalid room").await;
    }
    let Some(secret) = snapshot.room_secret.as_deref() else {
        return write_response(&mut socket, 403, "room secret is unavailable").await;
    };
    let Some(challenge) = challenge.filter(|value| is_lower_hex(value, CHALLENGE_HEX_LEN)) else {
        return write_response(&mut socket, 403, "invalid challenge").await;
    };
    let room_id = snapshot.room_id.clone().unwrap_or_default();
    if !verify_guest_auth(secret, &room_id, &challenge, auth.as_deref()) {
        return write_response(&mut socket, 403, "invalid invitation proof").await;
    }

    // Bind the proof to the address the guest actually reached. A peer that
    // only relays the host's answer to a different candidate IP cannot make
    // the guest accept itself as the host.
    let host_ip = match socket.local_addr() {
        Ok(addr) => addr.ip().to_string(),
        Err(_) => {
            return write_response(&mut socket, 500, "invalid local address").await;
        }
    };
    let Some(proof) = host_proof(
        secret,
        &room_id,
        &host_ip,
        &challenge,
        snapshot.minecraft_port,
    ) else {
        return write_response(&mut socket, 403, "room secret is invalid").await;
    };

    let body = serde_json::to_string(&VoidLinkStatus {
        room_id,
        mc_port: snapshot.minecraft_port,
        host_ip,
        challenge,
        proof,
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
    if body.len() > MAX_RESPONSE_BYTES {
        return write_small_error(socket, 500).await;
    }
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

async fn write_small_error(socket: &mut tokio::net::TcpStream, status: u16) -> std::io::Result<()> {
    let body = "error";
    let response = format!(
        "HTTP/1.1 {} Error\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        status,
        body.len(),
        body
    );
    socket.write_all(response.as_bytes()).await?;
    socket.flush().await
}

/// Tailscale addresses a VoidLink query may target. Invalid, duplicate and
/// non-tailnet literals are dropped; the order supplied by Tailscale is kept,
/// so callers can try every address of a peer regardless of IPv4/IPv6 order.
pub(crate) fn allowed_void_link_ips(ips: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for raw in ips {
        let Some(ip) = canonical_ip(raw) else {
            continue;
        };
        let Ok(parsed) = ip.parse::<std::net::IpAddr>() else {
            continue;
        };
        if is_allowed_void_link_ip(parsed) && !out.contains(&ip) {
            out.push(ip);
        }
    }
    out
}

/// VoidLink is a direct tailnet control channel. Only Tailscale's IPv4 CGNAT
/// range and its IPv6 ULA prefix are accepted; arbitrary IP literals are not
/// trusted merely because a caller supplied them.
fn is_allowed_void_link_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            let octets = v4.octets();
            octets[0] == 100 && (64..=127).contains(&octets[1])
        }
        std::net::IpAddr::V6(v6) => {
            let segments = v6.segments();
            segments[0] == 0xfd7a && segments[1] == 0x115c && segments[2] == 0xa1e0
        }
    }
}

async fn query_host_status_with(
    host_ip: &str,
    port: u16,
    room_id: &str,
    room_secret: &str,
    allow_loopback_for_test: bool,
) -> crate::error::Result<VoidLinkStatus> {
    let ip: std::net::IpAddr = host_ip.parse().map_err(|_| {
        crate::error::LauncherError::Download(format!(
            "VoidLink: invalid host address '{}'",
            host_ip
        ))
    })?;
    let allowed = is_allowed_void_link_ip(ip)
        || (allow_loopback_for_test && ip.is_loopback() && ip.is_ipv4());
    if !allowed {
        return Err(crate::error::LauncherError::Download(
            "VoidLink: host address is not a Tailscale address".to_string(),
        ));
    }
    let host = match ip {
        std::net::IpAddr::V4(v4) => v4.to_string(),
        std::net::IpAddr::V6(v6) => format!("[{}]", v6),
    };
    let expected_host_ip = ip.to_string();
    let room = canonical_room_id(room_id);
    if room.is_empty() {
        return Err(crate::error::LauncherError::Download(
            "VoidLink: empty room id".to_string(),
        ));
    }
    let challenge = generate_challenge();
    let auth = guest_auth(room_secret, &room, &challenge).ok_or_else(|| {
        crate::error::LauncherError::Auth(
            "VoidLink: the room invitation secret is invalid".to_string(),
        )
    })?;

    // No redirects: a room peer must not be able to turn the control query
    // into an SSRF probe against an arbitrary internal endpoint.
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(crate::error::LauncherError::Network)?;
    let url = format!(
        "http://{}:{}/room/status?room={}&challenge={}&auth={}",
        host,
        port,
        urlencoding::encode(&room),
        urlencoding::encode(&challenge),
        urlencoding::encode(&auth)
    );
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(crate::error::LauncherError::Network)?;
    if resp.status() != reqwest::StatusCode::OK {
        return Err(crate::error::LauncherError::Download(format!(
            "VoidLink: host returned {}",
            resp.status()
        )));
    }
    if let Some(len) = resp.content_length() {
        if len > MAX_RESPONSE_BYTES as u64 {
            return Err(crate::error::LauncherError::Download(
                "VoidLink: response is too large".to_string(),
            ));
        }
    }
    let mut stream = resp.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(crate::error::LauncherError::Network)?;
        if chunk.len() > MAX_RESPONSE_BYTES - bytes.len() {
            return Err(crate::error::LauncherError::Download(
                "VoidLink: response is too large".to_string(),
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    let status: VoidLinkStatus =
        serde_json::from_slice(&bytes).map_err(crate::error::LauncherError::Json)?;

    // Every field that selects the endpoint is authenticated. A foreign peer
    // cannot replay an old proof (the challenge is fresh), change the port, or
    // claim the real host's answer as its own IP.
    if !status.room_id.eq_ignore_ascii_case(&room) {
        return Err(crate::error::LauncherError::Download(
            "VoidLink: the response is for another room".to_string(),
        ));
    }
    if status.challenge != challenge {
        return Err(crate::error::LauncherError::Download(
            "VoidLink: the response challenge does not match".to_string(),
        ));
    }
    if canonical_ip(&status.host_ip).as_deref() != Some(expected_host_ip.as_str()) {
        return Err(crate::error::LauncherError::Download(
            "VoidLink: the response was not produced by this host".to_string(),
        ));
    }
    if !verify_host_proof(
        room_secret,
        &room,
        &expected_host_ip,
        &challenge,
        status.mc_port,
        &status.proof,
    ) {
        return Err(crate::error::LauncherError::Download(
            "VoidLink: the host proof is invalid".to_string(),
        ));
    }
    Ok(status)
}

/// Guest-side client: query the host bridge with a fresh challenge and verify
/// the cryptographic proof before returning the advertised endpoint.
pub async fn query_host_status(
    host_ip: &str,
    port: u16,
    room_id: &str,
    room_secret: &str,
) -> crate::error::Result<VoidLinkStatus> {
    query_host_status_with(host_ip, port, room_id, room_secret, false).await
}

#[cfg(test)]
pub(crate) async fn query_host_status_for_test(
    host_ip: &str,
    port: u16,
    room_id: &str,
    room_secret: &str,
) -> crate::error::Result<VoidLinkStatus> {
    query_host_status_with(host_ip, port, room_id, room_secret, true).await
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
    // Validate the requested address BEFORE touching a live bridge: an
    // invalid/missing Tailscale address must not stop a bridge that is still
    // serving the room.
    let port = room_state.void_link_port();
    let addr = bind_addr(&self_ip, port).ok_or_else(|| {
        crate::error::LauncherError::Launch(format!(
            "Invalid tailnet IP for the bridge: {}",
            self_ip
        ))
    })?;

    // Serialize start/stop transitions. The bridge-state mutex itself must not
    // be held across `bind().await` (the Tauri command future must stay
    // `Send`), but the operation lock may be: tokio's mutex guard is Send.
    let _op = void_link_op_lock().lock().await;

    // A status poll can race a leave. Never recreate a bridge after this
    // launcher stopped being the host.
    if room_state.role() != RoomRole::Host {
        return Ok(());
    }

    let old_bridge = {
        let mut state = void_link_state().lock().unwrap_or_else(|e| e.into_inner());
        match state.as_ref() {
            Some(b) if b.ip == self_ip && !b.task.is_finished() => {
                return Ok(());
            }
            Some(_) => state.take(),
            None => None,
        }
    }; // bridge-state guard dropped here, before the await

    if let Some(old) = old_bridge {
        // Abort AND await the old task: abort() only schedules cancellation,
        // and the old listener must be dropped before a rebind on the same
        // port can succeed.
        if !old.task.is_finished() {
            old.task.abort();
        }
        let _ = old.task.await;
    }

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
    let mut state = void_link_state().lock().unwrap_or_else(|e| e.into_inner());
    *state = Some(BridgeTask { ip: self_ip, task });
    Ok(())
}

/// Abort the host-side bridge (the room ended). The bridge never outlives a
/// room session; the next `ensure_void_link_server` binds a fresh listener on
/// the current tailnet IP.
///
/// Async because it must wait for an in-flight start to finish before taking
/// the bridge; otherwise a stop that runs while `ensure` is inside
/// `bind().await` would see `None` and the new listener would be installed
/// after the stop.
pub async fn stop_void_link_server() {
    let _op = void_link_op_lock().lock().await;
    let old = {
        let mut state = void_link_state().lock().unwrap_or_else(|e| e.into_inner());
        state.take()
    };
    if let Some(old) = old {
        // Wait for cancellation so the listener is actually closed before the
        // caller reports the room as stopped (and before a later rebind).
        if !old.task.is_finished() {
            old.task.abort();
        }
        let _ = old.task.await;
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
    fn authorize_room_matches_public_id_case_insensitively() {
        assert!(authorize_room(Some("ABCD-1234"), Some("abcd-1234")));
        assert!(authorize_room(Some(" abcd-1234 "), Some("ABCD-1234")));
        assert!(!authorize_room(Some("ABCD-1234"), Some("WXYZ-9012")));
        assert!(!authorize_room(None, Some("ABCD-1234")));
        assert!(!authorize_room(Some(""), Some("ABCD-1234")));
        assert!(!authorize_room(Some("ABCD-1234"), None));
    }

    fn test_secret() -> String {
        crate::rooms::room_state::generate_room_secret()
    }

    #[test]
    fn host_proof_binds_room_host_challenge_and_port() {
        let secret = test_secret();
        let challenge = generate_challenge();
        let proof =
            host_proof(&secret, "ABCD-1234", "100.64.0.9", &challenge, Some(45565)).expect("proof");

        assert!(verify_host_proof(
            &secret,
            "abcd-1234",
            "100.64.0.9",
            &challenge,
            Some(45565),
            &proof
        ));
        assert!(!verify_host_proof(
            &secret,
            "WXYZ-9012",
            "100.64.0.9",
            &challenge,
            Some(45565),
            &proof
        ));
        assert!(!verify_host_proof(
            &secret,
            "ABCD-1234",
            "100.64.0.8",
            &challenge,
            Some(45565),
            &proof
        ));
        assert!(!verify_host_proof(
            &test_secret(),
            "ABCD-1234",
            "100.64.0.9",
            &challenge,
            Some(45565),
            &proof
        ));
        assert!(!verify_host_proof(
            &secret,
            "ABCD-1234",
            "100.64.0.9",
            &challenge,
            Some(45566),
            &proof
        ));
        let other_challenge = generate_challenge();
        assert!(
            !verify_host_proof(
                &secret,
                "ABCD-1234",
                "100.64.0.9",
                &other_challenge,
                Some(45565),
                &proof
            ),
            "a captured proof must not replay for a fresh challenge"
        );
    }

    #[test]
    fn allowed_void_link_ips_keep_both_families_in_either_order() {
        let v4 = "100.64.0.9".to_string();
        let v6 = "fd7a:115c:a1e0::1".to_string();
        assert_eq!(
            allowed_void_link_ips(&[v4.clone(), v6.clone()]),
            vec![v4.clone(), v6.clone()]
        );
        assert_eq!(
            allowed_void_link_ips(&[v6.clone(), v4.clone()]),
            vec![v6.clone(), v4.clone()],
            "the order Tailscale reports must be preserved"
        );
        assert_eq!(allowed_void_link_ips(std::slice::from_ref(&v4)), vec![v4]);
        assert_eq!(allowed_void_link_ips(std::slice::from_ref(&v6)), vec![v6]);
    }

    #[test]
    fn allowed_void_link_ips_drop_invalid_duplicate_and_foreign_addresses() {
        let valid = "100.64.0.9".to_string();
        assert_eq!(
            allowed_void_link_ips(&[
                valid.clone(),
                valid.clone(),
                "8.8.8.8".to_string(),
                "not-an-ip".to_string(),
                "127.0.0.1".to_string(),
                "fd7a:115c:a1e0::2".to_string(),
            ]),
            vec![valid, "fd7a:115c:a1e0::2".to_string()]
        );
    }

    #[test]
    fn guest_auth_requires_the_invitation_secret() {
        let secret = test_secret();
        let challenge = generate_challenge();
        let auth = guest_auth(&secret, "ABCD-1234", &challenge).expect("auth");
        assert!(verify_guest_auth(
            &secret,
            "abcd-1234",
            &challenge,
            Some(&auth)
        ));
        assert!(!verify_guest_auth(
            &test_secret(),
            "ABCD-1234",
            &challenge,
            Some(&auth)
        ));
        assert!(!verify_guest_auth(
            &secret,
            "WXYZ-9012",
            &challenge,
            Some(&auth)
        ));
        assert!(!verify_guest_auth(&secret, "ABCD-1234", &challenge, None));
    }

    #[tokio::test]
    async fn connection_limit_admits_exactly_the_configured_number() {
        let limit = ConnectionLimit::new(MAX_CONNECTIONS);
        let mut permits = Vec::new();
        for _ in 0..MAX_CONNECTIONS {
            permits.push(limit.try_admit().expect("permit within the limit"));
        }
        assert_eq!(limit.available(), 0);
        assert!(
            limit.try_admit().is_none(),
            "connection MAX_CONNECTIONS + 1 must be rejected"
        );

        permits.pop();
        assert!(
            limit.try_admit().is_some(),
            "releasing one permit must admit a new connection"
        );
        drop(permits);
        assert_eq!(
            limit.available(),
            MAX_CONNECTIONS,
            "all permits must return after the connections end"
        );
    }

    #[tokio::test]
    async fn connection_limit_releases_permits_on_error_and_timeout() {
        let limit = ConnectionLimit::new(1);

        // Error path: the guard is dropped when the connection task returns an
        // error, so the slot is immediately reusable.
        let failed: std::io::Result<()> = {
            let _permit = limit.try_admit().expect("permit");
            Err(std::io::Error::other("simulated connection error"))
        };
        assert!(failed.is_err());
        assert!(limit.try_admit().is_some(), "error must release the permit");
        drop(limit.try_admit());

        // Timeout path: dropping the timed-out future also drops its guard.
        let timed_out = tokio::time::timeout(Duration::from_millis(20), async {
            let _permit = limit.try_admit().expect("permit");
            std::future::pending::<()>().await;
        })
        .await;
        assert!(timed_out.is_err(), "the request must time out");
        assert!(
            limit.try_admit().is_some(),
            "timeout must release the permit"
        );
    }

    #[tokio::test]
    async fn connection_limit_recycles_without_leaking_slots() {
        let limit = ConnectionLimit::new(MAX_CONNECTIONS);
        for _ in 0..1000 {
            let permit = limit.try_admit().expect("permit");
            drop(permit);
        }
        assert_eq!(limit.available(), MAX_CONNECTIONS);
    }

    #[test]
    fn bind_addr_parses_tailnet_ip() {
        let addr = bind_addr("100.101.102.103", VOID_LINK_PORT).unwrap();
        assert_eq!(addr.to_string(), "100.101.102.103:48888");
        assert!(bind_addr("not-an-ip", VOID_LINK_PORT).is_none());
    }

    #[test]
    fn query_param_extracts_the_code() {
        assert_eq!(
            query_param("code=ABCD-1234", "code").as_deref(),
            Some("ABCD-1234")
        );
        assert_eq!(
            query_param("x=1&code=ABCD-1234&y=2", "code").as_deref(),
            Some("ABCD-1234")
        );
        assert_eq!(query_param("x=1", "code"), None);
        assert_eq!(query_param("", "code"), None);
    }

    #[test]
    fn void_link_only_accepts_tailscale_addresses() {
        for allowed in [
            "100.64.0.1",
            "100.100.100.100",
            "100.127.255.254",
            "fd7a:115c:a1e0::1",
            "fd7a:115c:a1e0:1234::abcd",
        ] {
            let ip: std::net::IpAddr = allowed.parse().unwrap();
            assert!(
                is_allowed_void_link_ip(ip),
                "{allowed} is a Tailscale address"
            );
        }
        for denied in [
            "127.0.0.1",
            "10.0.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.63.255.255",
            "100.128.0.1",
            "8.8.8.8",
            "::1",
            "fe80::1",
            "fd00::1",
            "fd7a:115c:a1e1::1",
        ] {
            let ip: std::net::IpAddr = denied.parse().unwrap();
            assert!(
                !is_allowed_void_link_ip(ip),
                "{denied} must not be treated as a tailnet host"
            );
        }
    }

    #[tokio::test]
    async fn query_host_status_rejects_non_tailnet_targets_before_connecting() {
        // Loopback/private/link-local targets are refused before a client is
        // built, so this never reaches the network.
        for host in [
            "127.0.0.1",
            "10.0.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "::1",
            "fe80::1",
            "not-an-ip",
            "100.100.100.100.evil.example",
        ] {
            assert!(
                query_host_status(host, 1, "ABCD-1234", &test_secret())
                    .await
                    .is_err(),
                "non-tailnet endpoint must be rejected: {host}"
            );
        }
    }

    #[tokio::test]
    async fn client_rejects_an_oversized_control_response() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind fake host");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            if let Ok((mut socket, _)) = listener.accept().await {
                let body = vec![b'x'; MAX_RESPONSE_BYTES + 1];
                // No Content-Length: the client must enforce its own cap while
                // streaming, not trust a declared size.
                let head = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n";
                let _ = socket.write_all(head.as_bytes()).await;
                let _ = socket.write_all(&body).await;
                let _ = socket.shutdown().await;
            }
        });
        let result =
            query_host_status_for_test("127.0.0.1", addr.port(), "ABCD-1234", &test_secret()).await;
        assert!(result.is_err(), "oversized response must be rejected");
    }

    #[tokio::test]
    async fn client_rejects_a_status_without_a_valid_host_proof() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind fake host");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            if let Ok((mut socket, _)) = listener.accept().await {
                let mut request = vec![0u8; 2048];
                let read = socket.read(&mut request).await.unwrap_or(0);
                let text = String::from_utf8_lossy(&request[..read]).into_owned();
                let target = text
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or("");
                let (_, query) = target.split_once('?').unwrap_or((target, ""));
                let challenge = query_param(query, "challenge").unwrap_or_default();
                let body = serde_json::json!({
                    "room_id": "ABCD-1234",
                    "mc_port": 45565,
                    "host_ip": "127.0.0.1",
                    "challenge": challenge,
                    "proof": "0".repeat(PROOF_HEX_LEN),
                })
                .to_string();
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = socket.write_all(head.as_bytes()).await;
                let _ = socket.write_all(body.as_bytes()).await;
                let _ = socket.shutdown().await;
            }
        });
        let result =
            query_host_status_for_test("127.0.0.1", addr.port(), "ABCD-1234", &test_secret()).await;
        assert!(
            result.is_err(),
            "a peer that does not know the invitation secret must be rejected"
        );
    }

    #[tokio::test]
    async fn discovery_request_never_contains_the_invitation_secret() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind fake host");
        let addr = listener.local_addr().expect("addr");
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            if let Ok((mut socket, _)) = listener.accept().await {
                let mut request = vec![0u8; MAX_REQUEST_BYTES];
                let read = socket.read(&mut request).await.unwrap_or(0);
                let text = String::from_utf8_lossy(&request[..read]).into_owned();
                let _ = tx.send(text);
                let _ = socket
                    .write_all(
                        b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .await;
            }
        });

        let secret = test_secret();
        let result =
            query_host_status_for_test("127.0.0.1", addr.port(), "ABCD-1234", &secret).await;
        assert!(result.is_err(), "the fake host did not prove the room");
        let request = rx.await.expect("request captured");
        assert!(
            !request.contains(&secret),
            "the invitation secret must never leave the guest"
        );
        assert!(request.contains("room=ABCD-1234"));
        assert!(request.contains("challenge="));
        assert!(request.contains("auth="));
    }

    #[tokio::test]
    async fn idle_connection_is_closed_by_the_request_timeout() {
        let dir = std::env::temp_dir().join(format!(
            "vl_void_link_timeout_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let room = Arc::new(RoomStateManager::new(&dir));
        room.create_room(None).expect("host room");
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind bridge");
        let port = listener.local_addr().expect("addr").port();
        tokio::spawn(void_link_server(listener, room));

        let mut socket = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .expect("connect");
        // Send nothing: the server must close this connection after the
        // timeout instead of retaining a task forever.
        tokio::time::sleep(REQUEST_TIMEOUT + Duration::from_millis(100)).await;
        let mut buf = [0u8; 1];
        let read = tokio::time::timeout(Duration::from_secs(1), socket.read(&mut buf))
            .await
            .expect("server should close the idle socket");
        assert!(
            matches!(read, Ok(0) | Err(_)),
            "idle connection should be closed, got: {read:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn oversized_request_is_rejected_without_waiting_for_the_timeout() {
        let dir = std::env::temp_dir().join(format!(
            "vl_void_link_big_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let room = Arc::new(RoomStateManager::new(&dir));
        room.create_room(None).expect("host room");
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind bridge");
        let port = listener.local_addr().expect("addr").port();
        tokio::spawn(void_link_server(listener, room));
        let mut socket = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .expect("connect");
        let prefix = "GET /room/status?room=ABCD-1234 HTTP/1.1\r\nX: ";
        let request = format!("{}{}", prefix, "a".repeat(2048 - prefix.len()));
        socket.write_all(request.as_bytes()).await.expect("write");
        let mut response = String::new();
        socket.read_to_string(&mut response).await.expect("read");
        assert!(response.contains("431"), "got: {response}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn finds_header_terminator_across_buffer() {
        let data = b"GET /room/status?room=ABCD-1234 HTTP/1.1\r\nHost: x\r\n\r\n";
        assert_eq!(find_subslice(data, b"\r\n\r\n"), Some(data.len() - 4));
        assert_eq!(find_subslice(b"GET / HTTP/1.1\r\n", b"\r\n\r\n"), None);
    }
}

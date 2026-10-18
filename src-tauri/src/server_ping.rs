// ==================== Server List Ping (SLP) ====================
// Implements the vanilla Java Edition status handshake so the launcher can
// show an honest server status (players, latency) without ever joining the
// game. Protocol: VarInt-framed packets — handshake (protocol version -1,
// "just asking for status") -> status request -> status response (JSON),
// then the dedicated 0x01 ping/echo pair for a true ping round trip.
//
// Only addresses already present in the instance's servers.dat are pinged:
// the command resolves the address against the saved list first, so this
// module never becomes an arbitrary host:port probe reachable from the
// webview. Public servers behind SRV records (`_minecraft._tcp`) are not
// resolved here — the status may therefore fail while the game can still
// join; that limitation is intentional (no extra DNS dependencies).

use serde::Serialize;
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

/// Vanilla default port when the address has no `:port`.
pub const DEFAULT_PORT: u16 = 25565;
/// Hard budget for the whole exchange (resolve + connect + status + ping).
pub const PING_BUDGET: Duration = Duration::from_secs(5);
/// Guard for VarInt-prefixed strings (status JSON is far below this).
const MAX_STRING_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PingState {
    /// A valid status response was received (players are trustworthy).
    Online,
    /// The server did not answer within the time budget.
    Timeout,
    /// The host could not be reached (DNS failure, connection refused...).
    Unreachable,
    /// We reached something, but the answer was not a valid status response.
    Error,
}

/// Result of one status check, as sent to the frontend.
#[derive(Debug, Clone, Serialize)]
pub struct PingResult {
    pub state: PingState,
    /// Round trip of the ping/echo pair, or of the status exchange when the
    /// server does not answer the dedicated ping.
    pub ping_ms: Option<u32>,
    pub players_online: Option<u32>,
    pub players_max: Option<u32>,
    /// Version string the server reports (not the instance's version).
    pub version_name: Option<String>,
    /// Diagnostic text (why it failed); not shown as a number by the UI.
    pub detail: Option<String>,
}

impl PingResult {
    fn fail(state: PingState, detail: impl Into<String>) -> Self {
        Self {
            state,
            ping_ms: None,
            players_online: None,
            players_max: None,
            version_name: None,
            detail: Some(detail.into()),
        }
    }
}

/// Split a saved server address into `(host, port)`, mirroring the vanilla
/// client: `host`, `host:port`, `[ipv6]` or `[ipv6]:port`; a bare IPv6
/// literal without brackets is a host (its colons are not a port).
pub fn parse_host_port(address: &str) -> Result<(String, u16), String> {
    let addr = address.trim();
    if addr.is_empty() {
        return Err("empty address".to_string());
    }
    if let Some(rest) = addr.strip_prefix('[') {
        let end = rest
            .find(']')
            .ok_or_else(|| "missing ']' in IPv6 address".to_string())?;
        let host = &rest[..end];
        if host.is_empty() {
            return Err("empty host".to_string());
        }
        let tail = &rest[end + 1..];
        let port = if tail.is_empty() {
            DEFAULT_PORT
        } else {
            let text = tail
                .strip_prefix(':')
                .ok_or_else(|| "unexpected text after ']'".to_string())?;
            parse_port(text)?
        };
        return Ok((host.to_string(), port));
    }
    if addr.matches(':').count() == 1 {
        let (host, text) = addr.split_once(':').expect("one colon checked above");
        if host.is_empty() {
            return Err("empty host".to_string());
        }
        return Ok((host.to_string(), parse_port(text)?));
    }
    Ok((addr.to_string(), DEFAULT_PORT))
}

fn parse_port(text: &str) -> Result<u16, String> {
    let port: u32 = text
        .parse()
        .map_err(|_| format!("invalid port: {text:?}"))?;
    if port == 0 || port > u16::MAX as u32 {
        return Err(format!("port out of range: {port}"));
    }
    Ok(port as u16)
}

fn write_varint(buf: &mut Vec<u8>, value: i32) {
    let mut value = value as u32;
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            buf.push(byte);
            break;
        }
        buf.push(byte | 0x80);
    }
}

async fn read_varint<R: AsyncRead + Unpin>(reader: &mut R) -> std::io::Result<i32> {
    let mut result: u32 = 0;
    for shift in 0..5 {
        let byte = reader.read_u8().await?;
        result |= u32::from(byte & 0x7f) << (7 * shift);
        if byte & 0x80 == 0 {
            return Ok(result as i32);
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        "VarInt is too long",
    ))
}

async fn read_string<R: AsyncRead + Unpin>(reader: &mut R) -> std::io::Result<String> {
    let len = read_varint(reader).await?;
    if len < 0 || len as usize > MAX_STRING_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("status string length out of range: {len}"),
        ));
    }
    let mut buf = vec![0u8; len as usize];
    reader.read_exact(&mut buf).await?;
    String::from_utf8(buf).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "status string is not UTF-8",
        )
    })
}

/// Ping a server with the default time budget.
pub async fn ping_server(host: &str, port: u16) -> PingResult {
    ping_server_with_budget(host, port, PING_BUDGET).await
}

/// Ping a server with an explicit budget (tests use a small one).
pub async fn ping_server_with_budget(host: &str, port: u16, budget: Duration) -> PingResult {
    match timeout(budget, exchange(host, port)).await {
        Ok(result) => result,
        Err(_) => PingResult::fail(PingState::Timeout, "no response within the time budget"),
    }
}

/// Player counts and version reported by the server.
struct StatusFields {
    online: Option<u32>,
    max: Option<u32>,
    version: Option<String>,
}

fn as_count(value: Option<&serde_json::Value>) -> Option<u32> {
    let value = value?;
    value
        .as_u64()
        .or_else(|| value.as_f64().map(|f| f.max(0.0) as u64))
        .map(|v| v.min(u32::MAX as u64) as u32)
}

fn parse_status(text: &str) -> Option<StatusFields> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let players = value.get("players");
    let online = as_count(players.and_then(|p| p.get("online")));
    let max = as_count(players.and_then(|p| p.get("max")));
    let version = value
        .get("version")
        .and_then(|v| v.get("name"))
        .and_then(|n| n.as_str())
        .map(str::to_string);
    Some(StatusFields {
        online,
        max,
        version,
    })
}

async fn exchange(host: &str, port: u16) -> PingResult {
    // Resolve first so DNS failures read as "unreachable" instead of sitting
    // in the connect path until the whole budget expires.
    let addrs = match tokio::net::lookup_host((host, port)).await {
        Ok(addrs) => addrs.collect::<Vec<_>>(),
        Err(e) => return PingResult::fail(PingState::Unreachable, format!("dns: {e}")),
    };
    if addrs.is_empty() {
        return PingResult::fail(PingState::Unreachable, "no addresses resolved");
    }
    let mut stream = None;
    let mut last_error = String::new();
    for addr in addrs {
        match TcpStream::connect(addr).await {
            Ok(s) => {
                stream = Some(s);
                break;
            }
            Err(e) => last_error = e.to_string(),
        }
    }
    let Some(mut stream) = stream else {
        return PingResult::fail(PingState::Unreachable, format!("connect: {last_error}"));
    };

    // Handshake: packet id 0x00, protocol -1 (status only), host, port,
    // next state 1 (status). Sent together with the status request (0x00).
    let mut handshake = Vec::new();
    write_varint(&mut handshake, 0x00);
    write_varint(&mut handshake, -1);
    write_varint(&mut handshake, host.len() as i32);
    handshake.extend_from_slice(host.as_bytes());
    handshake.extend_from_slice(&port.to_be_bytes());
    write_varint(&mut handshake, 0x01);
    let mut packet = Vec::new();
    write_varint(&mut packet, handshake.len() as i32);
    packet.extend_from_slice(&handshake);
    packet.extend_from_slice(&[0x01, 0x00]);
    if let Err(e) = stream.write_all(&packet).await {
        return PingResult::fail(PingState::Unreachable, format!("write: {e}"));
    }

    // Status response: [length][packet id 0x00][json string].
    let started = Instant::now();
    let len = match read_varint(&mut stream).await {
        Ok(len) => len,
        Err(e) => {
            // Connected, but closed before saying anything meaningful.
            return PingResult::fail(PingState::Unreachable, format!("closed before status: {e}"));
        }
    };
    if !(0..=MAX_STRING_BYTES as i32).contains(&len) {
        return PingResult::fail(PingState::Error, format!("bad status length: {len}"));
    }
    let packet_id = match read_varint(&mut stream).await {
        Ok(id) => id,
        Err(e) => return PingResult::fail(PingState::Error, format!("status id: {e}")),
    };
    if packet_id != 0x00 {
        return PingResult::fail(
            PingState::Error,
            format!("unexpected packet id: {packet_id}"),
        );
    }
    let json = match read_string(&mut stream).await {
        Ok(json) => json,
        Err(e) => return PingResult::fail(PingState::Error, format!("status body: {e}")),
    };
    let status_rtt_ms = started.elapsed().as_millis().min(u128::from(u32::MAX)) as u32;
    let Some(fields) = parse_status(&json) else {
        return PingResult::fail(PingState::Error, "status body is not JSON");
    };

    // True latency comes from the ping/echo pair; servers that skip it fall
    // back to the status exchange round trip.
    let ping_ms = measure_ping(&mut stream).await.unwrap_or(status_rtt_ms);

    PingResult {
        state: PingState::Online,
        ping_ms: Some(ping_ms),
        players_online: fields.online,
        players_max: fields.max,
        version_name: fields.version,
        detail: None,
    }
}

/// Sends the dedicated 0x01 ping and waits for the echo; returns the round
/// trip in milliseconds, or `None` when the server does not answer it.
async fn measure_ping(stream: &mut TcpStream) -> Option<u32> {
    let mut packet = Vec::new();
    write_varint(&mut packet, 9);
    write_varint(&mut packet, 0x01);
    packet.extend_from_slice(&[0u8; 8]);
    let started = Instant::now();
    stream.write_all(&packet).await.ok()?;
    let len = read_varint(stream).await.ok()?;
    if len != 9 {
        return None;
    }
    let packet_id = read_varint(stream).await.ok()?;
    if packet_id != 0x01 {
        return None;
    }
    let mut echoed = [0u8; 8];
    stream.read_exact(&mut echoed).await.ok()?;
    Some(started.elapsed().as_millis().min(u128::from(u32::MAX)) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[test]
    fn parses_plain_host() {
        assert_eq!(
            parse_host_port("play.example.com").unwrap(),
            ("play.example.com".to_string(), DEFAULT_PORT)
        );
    }

    #[test]
    fn parses_host_with_port_and_trims() {
        assert_eq!(
            parse_host_port("  example.com:25566 ").unwrap(),
            ("example.com".to_string(), 25566)
        );
        assert_eq!(
            parse_host_port("127.0.0.1:19132").unwrap(),
            ("127.0.0.1".to_string(), 19132)
        );
    }

    #[test]
    fn parses_bracketed_ipv6() {
        assert_eq!(
            parse_host_port("[2001:db8::1]").unwrap(),
            ("2001:db8::1".to_string(), DEFAULT_PORT)
        );
        assert_eq!(
            parse_host_port("[2001:db8::1]:25567").unwrap(),
            ("2001:db8::1".to_string(), 25567)
        );
    }

    #[test]
    fn bare_ipv6_is_a_host_without_port() {
        assert_eq!(
            parse_host_port("2001:db8::1").unwrap(),
            ("2001:db8::1".to_string(), DEFAULT_PORT)
        );
        assert_eq!(
            parse_host_port("::1").unwrap(),
            ("::1".to_string(), DEFAULT_PORT)
        );
    }

    #[test]
    fn rejects_malformed_addresses() {
        assert!(parse_host_port("").is_err());
        assert!(parse_host_port("   ").is_err());
        assert!(parse_host_port("host:0").is_err());
        assert!(parse_host_port("host:65536").is_err());
        assert!(parse_host_port("host:abc").is_err());
        assert!(parse_host_port("host:").is_err());
        assert!(parse_host_port(":25565").is_err());
        assert!(parse_host_port("[::1").is_err());
        assert!(parse_host_port("[]:25565").is_err());
        assert!(parse_host_port("[::1]junk").is_err());
    }

    #[test]
    fn varint_encoding_matches_spec() {
        let cases: &[(i32, &[u8])] = &[
            (0, &[0x00]),
            (1, &[0x01]),
            (127, &[0x7f]),
            (128, &[0x80, 0x01]),
            (25565, &[0xdd, 0xc7, 0x01]),
            (-1, &[0xff, 0xff, 0xff, 0xff, 0x0f]),
        ];
        for (value, expected) in cases {
            let mut buf = Vec::new();
            write_varint(&mut buf, *value);
            assert_eq!(&buf, expected, "encode {value}");
        }
    }

    #[tokio::test]
    async fn varint_roundtrip() {
        for value in [0i32, 1, 127, 128, 25565, 2_147_483_647, -1] {
            let mut buf = Vec::new();
            write_varint(&mut buf, value);
            let mut reader: &[u8] = &buf;
            assert_eq!(read_varint(&mut reader).await.unwrap(), value);
        }
    }

    #[tokio::test]
    async fn read_varint_rejects_overlong_input() {
        let bytes = [0xffu8, 0xff, 0xff, 0xff, 0xff, 0xff];
        let mut reader: &[u8] = &bytes;
        assert!(read_varint(&mut reader).await.is_err());
    }

    #[test]
    fn parses_status_json_variants() {
        let fields =
            parse_status(r#"{"version":{"name":"1.21.4"},"players":{"max":100,"online":7}}"#)
                .unwrap();
        assert_eq!(fields.online, Some(7));
        assert_eq!(fields.max, Some(100));
        assert_eq!(fields.version.as_deref(), Some("1.21.4"));

        // Some servers send counts as floats; both must survive.
        let fields = parse_status(r#"{"players":{"max":20.0,"online":3.0}}"#).unwrap();
        assert_eq!(fields.online, Some(3));
        assert_eq!(fields.max, Some(20));
        assert_eq!(fields.version, None);

        // Missing players section is valid (no count shown, still online).
        let fields = parse_status(r#"{"version":{"name":"x"}}"#).unwrap();
        assert_eq!(fields.online, None);

        assert!(parse_status("not json").is_none());
    }

    /// A one-shot server that answers the status handshake and echoes the
    /// ping with the given player counts.
    async fn spawn_mock_status_server(online: u32, max: u32) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            // Handshake packet (length-prefixed) then the status request.
            let len = read_varint(&mut socket).await.unwrap();
            let mut payload = vec![0u8; len as usize];
            socket.read_exact(&mut payload).await.unwrap();
            let mut request = [0u8; 2];
            socket.read_exact(&mut request).await.unwrap();
            assert_eq!(request, [0x01, 0x00]);
            let json = format!(
                r#"{{"version":{{"name":"1.21.4","protocol":769}},"players":{{"max":{max},"online":{online}}},"description":{{"text":"hi"}}}}"#
            );
            let mut body = vec![0x00];
            write_varint(&mut body, json.len() as i32);
            body.extend_from_slice(json.as_bytes());
            let mut out = Vec::new();
            write_varint(&mut out, body.len() as i32);
            out.extend_from_slice(&body);
            socket.write_all(&out).await.unwrap();
            // Ping/echo: same packet goes back.
            let mut ping = [0u8; 10];
            socket.read_exact(&mut ping).await.unwrap();
            socket.write_all(&ping).await.unwrap();
        });
        port
    }

    #[tokio::test]
    async fn pings_mock_server_and_reports_players() {
        let port = spawn_mock_status_server(7, 100).await;
        let result = ping_server_with_budget("127.0.0.1", port, Duration::from_secs(5)).await;
        assert_eq!(result.state, PingState::Online, "{:?}", result.detail);
        assert_eq!(result.players_online, Some(7));
        assert_eq!(result.players_max, Some(100));
        assert_eq!(result.version_name.as_deref(), Some("1.21.4"));
        assert!(result.ping_ms.is_some());
    }

    #[tokio::test]
    async fn times_out_when_server_is_silent() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            // Accept and hold the connection without ever answering.
            let (_socket, _) = listener.accept().await.unwrap();
            tokio::time::sleep(Duration::from_secs(3)).await;
        });
        let result = ping_server_with_budget("127.0.0.1", port, Duration::from_millis(300)).await;
        assert_eq!(result.state, PingState::Timeout);
        assert_eq!(result.players_online, None);
    }

    #[tokio::test]
    async fn unreachable_when_connection_is_refused() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let result = ping_server_with_budget("127.0.0.1", port, Duration::from_secs(5)).await;
        assert_eq!(result.state, PingState::Unreachable);
    }

    #[tokio::test]
    async fn error_when_answer_is_not_a_status() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            // Connected, but speaks a different protocol.
            socket.write_all(b"hello world").await.unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
        });
        let result = ping_server_with_budget("127.0.0.1", port, Duration::from_secs(5)).await;
        assert_eq!(result.state, PingState::Error, "{:?}", result.detail);
    }
}

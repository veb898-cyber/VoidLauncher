//! Single-machine Rooms E2E harness.
//!
//! This is a *harness* that exercises the Rooms control plane over **real TCP
//! sockets bound to `127.0.0.1`**. It validates:
//!   * room lifecycle: create → advertise port → leave → re-create;
//!   * the VoidLink HTTP bridge (hand-rolled server + reqwest client), port
//!     propagation and room-code authorization;
//!   * state persistence across a simulated app restart (new manager).
//!
//! It deliberately does NOT (and cannot) validate:
//!   * Tailscale install / login / real tailnet transport;
//!   * peer discovery against a live `tailscale status`;
//!   * two-machine joining, or Minecraft actually connecting.
//! Those require a second machine and an authenticated Tailscale account and
//! are tracked as BLOCKED in the E2E report.
//!
//! The bridge binds an **ephemeral** loopback port (not the production
//! `VOID_LINK_PORT`) so the harness is safe to run alongside the rest of the
//! suite — the `java` self-spawn test re-runs this binary as a child, and a
//! fixed port would collide with the parent's bridge.
//!
//! Run: `cargo test rooms::e2e -- --nocapture`

use super::minecraft_connector::{LanLogDiscovery, MinecraftDiscovery};
use super::peer_discovery::{peers_from_status, resolve_host_for_room};
use super::room_state::{valid_room_id, RoomRole, RoomStateManager};
use super::tailscale::{PeerNode, TailscaleStatus};
use super::void_link::{
    bind_addr, ensure_void_link_server, query_host_status, stop_void_link_server,
    void_link_server, VOID_LINK_PORT,
};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const HOST_IP: &str = "127.0.0.1";

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("vl_e2e_{}_{}_{}", tag, std::process::id(), nanos));
    std::fs::remove_dir_all(&dir).ok();
    dir
}

/// Grab an ephemeral loopback port by binding to port 0 and dropping the
/// listener. A tiny TOCTOU window with the real bind in the code under test
/// is acceptable for a local harness.
async fn pick_free_port() -> u16 {
    let l = tokio::net::TcpListener::bind((HOST_IP, 0)).await.expect("bind ephemeral");
    let port = l.local_addr().expect("local addr").port();
    drop(l);
    port
}

/// Send a raw HTTP/1.1 request over loopback and return (status_code, body).
async fn raw_http(port: u16, request: &str) -> (u16, String) {
    let mut stream = tokio::net::TcpStream::connect((HOST_IP, port))
        .await
        .expect("connect to bridge");
    stream.write_all(request.as_bytes()).await.expect("write request");
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.ok();
    let text = String::from_utf8_lossy(&buf).into_owned();
    let code = text
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse::<u16>().ok())
        .unwrap_or(0);
    let body = text.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
    (code, body)
}

/// One synthetic tailnet peer stamped like the host does (`<room>-host`).
fn hinted_host(room_id: &str) -> Vec<super::peer_discovery::PeerInfo> {
    let lower = room_id.to_ascii_lowercase();
    let peer = PeerNode {
        node_key: Some("nodekey:peer:host".into()),
        host_name: Some(format!("{}-host", lower)),
        dns_name: Some(format!("{}-host.tailnet.ts.net.", lower)),
        tailnet_ips: Some(vec!["100.64.0.10".into()]),
        online: Some(true),
        last_seen: Some("now".into()),
        sharee_node: Some(true),
        logged_in: Some(true),
        user_id: Some(1),
    };
    let status = TailscaleStatus {
        version: Some("1.102.4".into()),
        backend_state: Some("Running".into()),
        auth_url: None,
        current_tailnet: None,
        self_node: None,
        peer: Some(serde_json::to_value(vec![peer]).unwrap()),
        user: None,
        magic_dns_suffix: Some("tailnet.ts.net".into()),
    };
    peers_from_status(&status)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn room_control_plane_lifecycle_over_loopback() {
    // ---------- 0. Address logic (pure) ----------
    assert_eq!(
        bind_addr("100.101.102.103", VOID_LINK_PORT).map(|a| a.to_string()),
        Some("100.101.102.103:48888".to_string()),
        "bridge bind address"
    );
    assert!(bind_addr("not-an-ip", VOID_LINK_PORT).is_none(), "invalid ip rejected");

    // ---------- 1. HOST: create room ----------
    let host_dir = temp_dir("host");
    let host = Arc::new(RoomStateManager::new(&host_dir));
    let row = host.create_room(Some("E2E room".into())).expect("create_room");
    assert_eq!(row.role, RoomRole::Host, "role after create");
    let code = row.room_id.clone().expect("room id");
    assert!(valid_room_id(&code), "generated code shape: {}", code);
    println!("[e2e] host created room {}", code);

    // ---------- 2. HOST: start VoidLink bridge on an ephemeral loopback port ----------
    let listener = tokio::net::TcpListener::bind((HOST_IP, 0))
        .await
        .expect("bind ephemeral loopback port");
    let port = listener.local_addr().expect("local addr").port();
    tokio::spawn(void_link_server(listener, host.clone()));
    println!("[e2e] bridge listening on {}:{}", HOST_IP, port);

    // ---------- 3. GUEST: no port advertised yet ----------
    let pre = query_host_status(HOST_IP, port, &code)
        .await
        .expect("query before port");
    assert_eq!(pre.room_id, code, "bridge echoes room id");
    assert_eq!(pre.mc_port, None, "no port before Open to LAN");
    println!("[e2e] guest query pre-port -> mc_port=None");

    // ---------- 4. HOST: advertise the LAN port ----------
    host.set_minecraft_port(Some(45678)).expect("set port");
    let post = query_host_status(HOST_IP, port, &code)
        .await
        .expect("query after port");
    assert_eq!(post.mc_port, Some(45678), "guest sees advertised port");
    println!("[e2e] guest query post-port -> mc_port=Some(45678)");

    // ---------- 5. Auth: wrong / missing code rejected ----------
    assert!(
        query_host_status(HOST_IP, port, "ZZZZ-9999").await.is_err(),
        "wrong room code must be rejected"
    );
    let (code403, _) = raw_http(
        port,
        "GET /room/status HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert_eq!(code403, 403, "missing code must be 403");
    println!("[e2e] auth: wrong/missing code -> rejected");

    // ---------- 6. Protocol: method / route handling ----------
    let (m405, _) = raw_http(
        port,
        &format!(
            "POST /room/status?code={} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
            code
        ),
    )
    .await;
    assert_eq!(m405, 405, "non-GET must be 405");
    let (m404, _) = raw_http(
        port,
        "GET /nope HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert_eq!(m404, 404, "unknown route must be 404");
    println!("[e2e] protocol: POST=405, unknown route=404");

    // ---------- 7. HOST: leave -> bridge denies, room gone ----------
    host.leave().expect("leave");
    assert_eq!(host.role(), RoomRole::None, "role after leave");
    assert!(
        query_host_status(HOST_IP, port, &code).await.is_err(),
        "bridge must deny after the host left"
    );
    println!("[e2e] leave -> role=None, bridge denies");

    // ---------- 8. HOST: re-create -> new code, same live bridge ----------
    let again = host.create_room(None).expect("re-create");
    let code2 = again.room_id.clone().expect("new room id");
    assert!(valid_room_id(&code2), "re-created code shape: {}", code2);
    host.set_minecraft_port(Some(45679)).expect("set port 2");
    let reread = query_host_status(HOST_IP, port, &code2)
        .await
        .expect("query re-created room");
    assert_eq!(reread.mc_port, Some(45679), "re-created room serves new port");
    println!("[e2e] re-create {} -> bridge serves mc_port=45679", code2);

    // ---------- 9. Persistence across a simulated app restart ----------
    let reloaded = RoomStateManager::new(&host_dir);
    let snap = reloaded.snapshot();
    assert_eq!(snap.role, RoomRole::Host, "role survives restart");
    assert_eq!(snap.room_id.as_deref(), Some(code2.as_str()), "room id survives restart");
    // The advertised LAN port belongs to the world that was open before the
    // restart; it must NOT be advertised again (stale-port policy), and the
    // cleared value is written back to disk.
    assert_eq!(snap.minecraft_port, None, "stale port cleared on restart");
    let on_disk = RoomStateManager::new(&host_dir).snapshot();
    assert_eq!(on_disk.minecraft_port, None, "cleared port persisted");
    println!("[e2e] restart: role/room survive, stale port cleared");

    // ---------- 10. Discovery logic: hostname hint resolves the host ----------
    let peers = hinted_host(&code2);
    let resolved = resolve_host_for_room(&peers, Some(&code2)).expect("resolve host by hint");
    assert_eq!(resolved.ip(), Some("100.64.0.10"), "resolved host ip");
    let expected_dns = format!("{}-host.tailnet.ts.net", code2.to_ascii_lowercase());
    assert_eq!(resolved.dns(), Some(expected_dns.as_str()), "resolved host dns");
    println!("[e2e] discovery: hostname hint resolved peer");

    // ---------- 11. MC port parsing + version-aware join args ----------
    let log = "[12:00:00] [Server thread/INFO]: Local game hosted on port 45565";
    assert_eq!(LanLogDiscovery.detect_mc_port(log), Some(45565), "LAN port from log");
    assert_eq!(
        LanLogDiscovery.build_join_args("1.12.2", "100.64.0.10", 45565),
        Some(vec![
            "--server".to_string(),
            "100.64.0.10".to_string(),
            "--port".to_string(),
            "45565".to_string()
        ]),
        "legacy auto-join args"
    );
    assert_eq!(
        LanLogDiscovery.build_join_args("1.21.4", "100.64.0.10", 45565),
        Some(vec![
            "--quickPlayMultiplayer".to_string(),
            "100.64.0.10:45565".to_string()
        ]),
        "1.20+ auto-joins via Quick Play"
    );
    println!("[e2e] mc connector: port parse ok, legacy + quickplay join args ok");

    std::fs::remove_dir_all(&host_dir).ok();
    println!("[e2e] DONE (loopback control plane)");
}

/// Regression: a failed bridge start must leave the bridge manager empty,
/// otherwise every later Create Room silently no-ops and the guest never gets
/// a port. The invalid-IP path fails before opening any socket, so this test
/// never binds the fixed production port.
#[tokio::test]
async fn void_link_flag_recovers_after_start_error() {
    let dir = temp_dir("flag");
    let state = Arc::new(RoomStateManager::new(&dir));

    let first = ensure_void_link_server("not-an-ip".into(), state.clone()).await;
    assert!(first.is_err(), "invalid tailnet ip must fail");

    let second = ensure_void_link_server("also-not-an-ip".into(), state.clone()).await;
    assert!(
        second.is_err(),
        "manager must be empty after the first failure (a stuck bridge would return Ok)"
    );

    std::fs::remove_dir_all(&dir).ok();
    println!("[e2e] bridge manager empty after start error");
}

/// The host-side bridge follows the room session: idempotent for the same IP,
/// rebinds when the tailnet IP changes, and is torn down on leave so a stale
/// listener never outlives the room.
#[tokio::test]
async fn void_link_bridge_tracks_room_and_rebinds() {
    let dir = temp_dir("bridge");
    let state = Arc::new(RoomStateManager::new(&dir));
    let row = state.create_room(None).expect("create room");
    let code = row.room_id.expect("room id");

    let port_a = pick_free_port().await;
    state.set_void_link_port(port_a).unwrap();
    ensure_void_link_server(HOST_IP.into(), state.clone())
        .await
        .expect("first bind");
    let q = query_host_status(HOST_IP, port_a, &code).await.expect("bridge serves");
    assert_eq!(q.room_id, code, "first bind answers");

    // Same IP again → idempotent reuse, the live handler keeps answering.
    ensure_void_link_server(HOST_IP.into(), state.clone())
        .await
        .expect("idempotent same ip");
    let q2 = query_host_status(HOST_IP, port_a, &code).await.expect("still serves");
    assert_eq!(q2.room_id, code, "idempotent call must not kill the bridge");

    // IP change → the old listener is aborted and a fresh one answers on the
    // NEW address (rebind with the current tailnet IP).
    let port_b = pick_free_port().await;
    state.set_void_link_port(port_b).unwrap();
    ensure_void_link_server("127.0.0.2".into(), state.clone())
        .await
        .expect("rebind new ip");
    assert!(
        query_host_status(HOST_IP, port_a, &code).await.is_err(),
        "old listener must be aborted on IP change"
    );
    let q3 = query_host_status("127.0.0.2", port_b, &code).await.expect("new bridge serves");
    assert_eq!(q3.room_id, code, "rebound bridge answers");

    // Leave → the bridge never outlives the room session.
    state.leave().expect("leave");
    stop_void_link_server();
    assert!(
        query_host_status("127.0.0.2", port_b, &code).await.is_err(),
        "bridge must be stopped on leave"
    );

    std::fs::remove_dir_all(&dir).ok();
    println!("[e2e] bridge idempotent / rebind / torn down on leave");
}

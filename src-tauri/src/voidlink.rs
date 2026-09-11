//! VoidLink v0.1 — local TCP tunnel for VoidLauncher.
//!
//! Architecture (single Windows PC, prototype):
//!
//!   Minecraft Client --TCP--> VoidLink Client --TCP--> VoidLink Host --TCP--> Minecraft Server
//!
//! Every hop is a plain, unframed TCP byte stream, so Minecraft never knows a
//! tunnel exists. The Client binds a local port where Minecraft connects; the
//! Host binds a port the Client dials and forwards each connection to the real
//! Minecraft server.
//!
//! The simulated-network conditioner is applied *to writes into the tunnel*
//! on both endpoints, so each direction is impaired exactly once, at its
//! point of origin.
//!
//! Current version works only between local endpoints. Internet connectivity
//! and NAT traversal are intentionally not implemented yet.
//!
//! Terminology (important — not marketing, technical truth):
//!   - "latency" / "jitter": an artificial one-way delay added to chunks.
//!   - "loss" is NOT packet loss. Every "loss" event merely *stalls* the
//!     connection for `loss_stall_ms`, emulating TCP retransmission. All bytes
//!     are still delivered; stream integrity holds under every lossy profile.
//!   - "drop" (`--drop-percent`): a destructive stress mode — a chunk is
//!     silently discarded, which corrupts the byte stream on purpose. This is
//!     NOT a realistic simulation of network packet loss.

use std::fmt;
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::Duration;

/// Default address the Host listens on (the port the Client dials).
pub const DEFAULT_HOST_LISTEN: &str = "127.0.0.1:25588";
/// Default address of the local Minecraft server.
pub const DEFAULT_SERVER_ADDR: &str = "127.0.0.1:25565";
/// Default address the Client listens on (the port Minecraft dials).
pub const DEFAULT_CLIENT_LISTEN: &str = "127.0.0.1:25566";
/// Default address the standalone echo test server binds on.
pub const DEFAULT_TEST_SERVER_LISTEN: &str = "127.0.0.1:25589";

const CHUNK_SIZE: usize = 16 * 1024;

// ---------------------------------------------------------------------------
// Roles
// ---------------------------------------------------------------------------

/// Which end of the tunnel this process implements.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Accepts tunnel connections from the Client and dials the Minecraft server.
    Host,
    /// Accepts Minecraft connections and dials the Host.
    Client,
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Role::Host => f.write_str("host"),
            Role::Client => f.write_str("client"),
        }
    }
}

// ---------------------------------------------------------------------------
// Simulated-network profiles
// ---------------------------------------------------------------------------

/// Named stress/behaviour profiles for the simulated network.
///
/// Parameter justification (documented in docs/voidlink.md):
///   - `normal`:        clean local link, no artificial impairment.
///   - `high-latency`:  ~70 ms one-way latency with up to 15 ms jitter — a
///                      plausible high-ping link with some variance.
///   - `lossy`:         40 ms / 20 ms jitter plus 3 % "loss". Loss is NOT
///                      packet loss — it is a temporary *stall* (bad TCP
///                      queuing / retransmission); bytes are preserved.
///   - `very-bad`:      110 ms / 60 ms jitter, 6 % loss stalls, 500 ms stalls
///                      and a hard 128 kbps bandwidth cap — a barely-playable
///                      link, still intact because loss only stalls.
///   - `disconnect`:    like `normal`, but the link drops once 4 s after a
///                      session starts. Clients are expected to reconnect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    Normal,
    HighLatency,
    Lossy,
    VeryBad,
    Disconnect,
}

impl Profile {
    pub fn from_name(name: &str) -> Option<Profile> {
        match name {
            "normal" => Some(Profile::Normal),
            "high-latency" => Some(Profile::HighLatency),
            "lossy" => Some(Profile::Lossy),
            "very-bad" => Some(Profile::VeryBad),
            "disconnect" => Some(Profile::Disconnect),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Profile::Normal => "normal",
            Profile::HighLatency => "high-latency",
            Profile::Lossy => "lossy",
            Profile::VeryBad => "very-bad",
            Profile::Disconnect => "disconnect",
        }
    }

    pub fn params(self) -> ProfileParams {
        match self {
            Profile::Normal => ProfileParams::zero(),
            Profile::HighLatency => ProfileParams {
                latency_ms: 70,
                jitter_ms: 15,
                ..ProfileParams::zero()
            },
            Profile::Lossy => ProfileParams {
                latency_ms: 40,
                jitter_ms: 20,
                loss_percent: 3,
                loss_stall_ms: 250,
                ..ProfileParams::zero()
            },
            Profile::VeryBad => ProfileParams {
                latency_ms: 110,
                jitter_ms: 60,
                loss_percent: 6,
                loss_stall_ms: 500,
                bandwidth_kbps: 128,
                ..ProfileParams::zero()
            },
            Profile::Disconnect => ProfileParams {
                outage_after_ms: 4000,
                ..ProfileParams::zero()
            },
        }
    }
}

/// Tuning knobs of a simulated link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProfileParams {
    /// Fixed one-way latency added to each chunk.
    pub latency_ms: u64,
    /// Uniform jitter added on top of `latency_ms` (0..=jitter_ms).
    pub jitter_ms: u64,
    /// Chance (0-100) a chunk triggers a "loss" (stall, not byte drop).
    pub loss_percent: u32,
    /// How long a "loss" stalls the connection (ms).
    pub loss_stall_ms: u64,
    /// Destructive simulation: chance (0-100) a chunk is dropped entirely.
    /// Drops break stream integrity on purpose.
    pub drop_percent: u32,
    /// Bandwidth cap in kbit/s; 0 = unlimited.
    pub bandwidth_kbps: u64,
    /// Break a session once (ms after connection start); 0 = never.
    pub outage_after_ms: u64,
}

impl ProfileParams {
    pub const fn zero() -> ProfileParams {
        ProfileParams {
            latency_ms: 0,
            jitter_ms: 0,
            loss_percent: 0,
            loss_stall_ms: 0,
            drop_percent: 0,
            bandwidth_kbps: 0,
            outage_after_ms: 0,
        }
    }

    pub fn is_off(&self) -> bool {
        self.latency_ms == 0
            && self.jitter_ms == 0
            && self.loss_percent == 0
            && self.drop_percent == 0
            && self.bandwidth_kbps == 0
            && self.outage_after_ms == 0
    }

    pub fn from_profile_with_overrides(
        profile: Profile,
        overrides: &[(&str, u64)],
    ) -> Result<ProfileParams, String> {
        let mut p = profile.params();
        for (key, value) in overrides {
            match *key {
                "latency-ms" => p.latency_ms = *value,
                "jitter-ms" => p.jitter_ms = *value,
                "loss-percent" => p.loss_percent = *value as u32,
                "loss-stall-ms" => p.loss_stall_ms = *value,
                "drop-percent" => p.drop_percent = *value as u32,
                "bandwidth-kbps" => p.bandwidth_kbps = *value,
                "outage-after-ms" => p.outage_after_ms = *value,
                other => return Err(format!("unknown override key: {other}")),
            }
        }
        if p.loss_percent > 100 || p.drop_percent > 100 {
            return Err("loss/drop percentages must be 0-100".to_string());
        }
        Ok(p)
    }
}

// ---------------------------------------------------------------------------
// Small non-cryptographic RNG (deterministic seeds in tests; sims only)
// ---------------------------------------------------------------------------

/// xorshift64 — small, fast, non-cryptographic. Used only to make the
/// simulated network behave "randomly"; never for anything security related.
#[derive(Debug, Clone, Copy)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        // Zero is a fixed point of xorshift; force at least one set bit.
        Rng(seed | 0x9E37_79B9_7F4A_7C15)
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// Uniform value in `0..max` (max > 0).
    pub fn next_below(&mut self, max: u64) -> u64 {
        debug_assert!(max > 0);
        self.next_u64() % max
    }

    /// `true` with probability `percent / 100`.
    pub fn chance(&mut self, percent: u32) -> bool {
        if percent >= 100 {
            return true;
        }
        self.next_below(100) < percent as u64
    }
}

fn time_seed() -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    (std::process::id() as u64) ^ nanos.rotate_left(17)
}

// ---------------------------------------------------------------------------
// Conditioner
// ---------------------------------------------------------------------------

/// Applies the simulated-network impairments to one direction of a tunnel.
struct Conditioner {
    params: ProfileParams,
    rng: Rng,
    started: Instant,
    next_send_at: Instant,
    stall_until: Instant,
    outage_consumed: bool,
}

impl Conditioner {
    fn new(params: ProfileParams, rng: Rng, started: Instant) -> Conditioner {
        let now = Instant::now();
        Conditioner {
            params,
            rng,
            started,
            next_send_at: now,
            stall_until: now,
            outage_consumed: false,
        }
    }

    fn is_off(&self) -> bool {
        self.params.is_off()
    }

    /// Simulate one "lost" chunk by stalling the connection for a moment.
    /// Bytes are still delivered afterwards — stream integrity is preserved,
    /// but the link stutters like real TCP under loss.
    fn loss_roll(&mut self, now: Instant) {
        if self.params.loss_percent > 0 && self.rng.chance(self.params.loss_percent) {
            self.stall_until = now + Duration::from_millis(self.params.loss_stall_ms);
        }
    }

    /// Returns the delay to sleep before sending `byte_len` bytes, or `None`
    /// to send immediately. Combines latency+jitter, loss-stall and the
    /// bandwidth water-level clock.
    fn inject_delay(&mut self, now: Instant, byte_len: usize) -> Option<Duration> {
        if self.is_off() {
            return None;
        }
        let mut start_at = now;
        if self.stall_until > now {
            start_at = self.stall_until;
        }
        let jitter = if self.params.jitter_ms > 0 {
            self.rng.next_below(self.params.jitter_ms + 1)
        } else {
            0
        };
        start_at = start_at.max(now + Duration::from_millis(self.params.latency_ms + jitter));
        if self.params.bandwidth_kbps > 0 {
            // 1 kbit/s = 125 bytes/s
            let bytes_per_sec = self.params.bandwidth_kbps as f64 * 125.0;
            let chunk = Duration::from_secs_f64(byte_len as f64 / bytes_per_sec);
            self.next_send_at = self.next_send_at.max(now);
            start_at = start_at.max(self.next_send_at);
            self.next_send_at += chunk;
        }
        let delay = start_at.saturating_duration_since(now);
        if delay.is_zero() {
            None
        } else {
            Some(delay)
        }
    }

    /// Destructive mode: drop this chunk entirely (bytes silently disappear).
    fn should_drop(&mut self) -> bool {
        self.params.drop_percent > 0 && self.rng.chance(self.params.drop_percent)
    }

    /// Once per session, after `outage_after_ms` elapse, force the connection
    /// to drop. Returns true exactly once.
    fn should_break(&mut self, now: Instant) -> bool {
        if self.params.outage_after_ms > 0 && !self.outage_consumed {
            if now.duration_since(self.started) >= Duration::from_millis(self.params.outage_after_ms) {
                self.outage_consumed = true;
                return true;
            }
        }
        false
    }
}

// ---------------------------------------------------------------------------
// Stats & events
// ---------------------------------------------------------------------------

/// Live counters, readable at any moment (shared with tests and the CLI).
#[derive(Debug)]
pub struct RuntimeStats {
    pub sessions_total: AtomicU64,
    pub sessions_rejected: AtomicU64,
    pub sessions_errored: AtomicU64,
    pub idle_timeouts: AtomicU64,
    pub peak_concurrent: AtomicU64,
    pub active: AtomicU64,
    /// Bytes travelling towards the Minecraft server (client->server).
    pub bytes_upstream: AtomicU64,
    /// Bytes travelling back to the Minecraft client (server->client).
    pub bytes_downstream: AtomicU64,
    pub started: std::sync::Mutex<Instant>,
}

impl Default for RuntimeStats {
    fn default() -> Self {
        RuntimeStats {
            sessions_total: AtomicU64::new(0),
            sessions_rejected: AtomicU64::new(0),
            sessions_errored: AtomicU64::new(0),
            idle_timeouts: AtomicU64::new(0),
            peak_concurrent: AtomicU64::new(0),
            active: AtomicU64::new(0),
            bytes_upstream: AtomicU64::new(0),
            bytes_downstream: AtomicU64::new(0),
            started: std::sync::Mutex::new(Instant::now()),
        }
    }
}

impl RuntimeStats {
    fn track_session_start(&self) {
        let now_active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak_concurrent.fetch_max(now_active, Ordering::SeqCst);
    }

    fn track_session_end(&self) {
        self.active.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Human/CLI-facing lifecycle events, emitted from the run loop (single owner).
#[derive(Debug, Clone)]
pub enum TunnelEvent {
    Starting { role: Role },
    Listening { addr: SocketAddr },
    Connected { id: u64, peer: SocketAddr },
    Forwarding { id: u64, peer: SocketAddr },
    DialFailed { id: u64, peer: SocketAddr, error: String },
    Disconnected { id: u64, error: Option<String> },
    Stopped,
}

/// Idle-timeout increments live on `RuntimeStats`; this constant keeps the
/// CLI/log messages uniform.
pub const REASON_IDLE_TIMEOUT: &str = "idle timeout";

// ---------------------------------------------------------------------------
// Session plumbing
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct SessionReport {
    id: u64,
    error: Option<String>,
}

enum Side {
    ToTunnel,
    FromTunnel,
}

/// Copy `src` into `dst` until EOF, error, outage or idle time-out.
async fn relay_direction<R, W>(
    mut rx: R,
    mut tx: W,
    mut cond: Conditioner,
    idle_timeout: Option<Duration>,
) -> io::Result<u64>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buf = vec![0u8; CHUNK_SIZE];
    let mut sent: u64 = 0;
    loop {
        if cond.should_break(Instant::now()) {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionReset,
                "simulated outage: link dropped",
            ));
        }
        let n = match idle_timeout {
            Some(idle) => {
                let timer = tokio::time::sleep(idle);
                tokio::pin!(timer);
                tokio::select! {
                    r = rx.read(&mut buf) => r,
                    _ = &mut timer => {
                        return Err(io::Error::new(io::ErrorKind::TimedOut, REASON_IDLE_TIMEOUT));
                    }
                }
            }
            None => rx.read(&mut buf).await,
        };
        let n = n?;
        if n == 0 {
            break;
        }
        if !cond.is_off() {
            cond.loss_roll(Instant::now());
            if let Some(delay) = cond.inject_delay(Instant::now(), n) {
                tokio::time::sleep(delay).await;
            }
            if cond.should_drop() {
                continue;
            }
        }
        tx.write_all(&buf[..n]).await?;
        sent += n as u64;
    }
    let _ = tx.shutdown().await;
    Ok(sent)
}

/// Owns one tunneled connection: two relay tasks, teardown when either ends.
async fn handle_session(
    tunnel: TcpStream,
    local: TcpStream,
    role: Role,
    params: ProfileParams,
    idle_timeout: Option<Duration>,
    started: Instant,
    stats: Arc<RuntimeStats>,
    id: u64,
) -> SessionReport {
    let (tr, tw) = tunnel.into_split();
    let (lr, lw) = local.into_split();

    // The direction that WRITES INTO the tunnel is impaired (this is the
    // virtual WAN hop). The direction that writes out to the local Minecraft
    // side was already impaired once at the peer, so it stays clean.
    let seed = time_seed();
    let cond_into_tunnel = Conditioner::new(params, Rng::new(seed), started);
    let cond_to_local = Conditioner::new(ProfileParams::zero(), Rng::new(seed.wrapping_mul(3) | 1), started);

    stats.track_session_start();

    let mut set = JoinSet::new();
    // What flows into the tunnel socket (impaired).
    set.spawn(async move {
        let result = relay_direction(lr, tw, cond_into_tunnel, idle_timeout).await;
        (Side::ToTunnel, result)
    });
    // What flows out of the tunnel socket into the local Minecraft side.
    set.spawn(async move {
        let result = relay_direction(tr, lw, cond_to_local, idle_timeout).await;
        (Side::FromTunnel, result)
    });

    let mut to_tunnel: u64 = 0;
    let mut from_tunnel: u64 = 0;
    let mut error: Option<String> = None;

    // Wait for the first relay to finish (e.g. client sends EOF).
    if let Some(result) = set.join_next().await {
        collect(result, &mut to_tunnel, &mut from_tunnel, &mut error);
    }
    // Give the remaining relay up to 5 s to finish. Data may still be in
    // transit (e.g. bulk echo), or the server may be idle (disconnect).
    match tokio::time::timeout(Duration::from_secs(5), set.join_next()).await {
        Ok(Some(result)) => collect(result, &mut to_tunnel, &mut from_tunnel, &mut error),
        _ => {
            set.abort_all();
            while let Some(result) = set.join_next().await {
                collect(result, &mut to_tunnel, &mut from_tunnel, &mut error);
            }
            if error.is_none() {
                error = Some("timed out waiting for peer to close".to_string());
            }
        }
    }

    stats.track_session_end();
    if error.is_some() {
        stats.sessions_errored.fetch_add(1, Ordering::SeqCst);
    }
    if error.as_deref() == Some(REASON_IDLE_TIMEOUT) {
        stats.idle_timeouts.fetch_add(1, Ordering::SeqCst);
    }

    // Client: local Minecraft -> tunnel = upstream; tunnel -> local = downstream.
    // Host:   tunnel -> local server = upstream; local server -> tunnel = downstream.
    let (upstream, downstream) = match role {
        Role::Client => (to_tunnel, from_tunnel),
        Role::Host => (from_tunnel, to_tunnel),
    };
    stats.bytes_upstream.fetch_add(upstream, Ordering::SeqCst);
    stats.bytes_downstream.fetch_add(downstream, Ordering::SeqCst);

    SessionReport {
        id,
        error,
    }
}

fn collect(
    result: Result<(Side, io::Result<u64>), tokio::task::JoinError>,
    to_tunnel: &mut u64,
    from_tunnel: &mut u64,
    error: &mut Option<String>,
) {
    match result {
        Ok((Side::ToTunnel, Ok(bytes))) => *to_tunnel += bytes,
        Ok((Side::FromTunnel, Ok(bytes))) => *from_tunnel += bytes,
        Ok((_, Err(e))) => {
            if error.is_none() {
                *error = Some(e.to_string());
            }
        }
        Err(join_err) => {
            if error.is_none() {
                *error = Some(format!("relay task aborted: {join_err}"));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Options & entry points
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct RunOptions {
    /// Local socket to listen on. Must be a loopback address.
    pub listen: SocketAddr,
    /// Remote socket to dial per-connection.
    /// Host: the Minecraft server. Client: the Host's listen socket.
    pub peer: SocketAddr,
    pub params: ProfileParams,
    pub connect_timeout: Duration,
    pub idle_timeout: Option<Duration>,
}

/// Runs one tunnel endpoint until `stop` is set; reports lifecycle in
/// `on_event`. `stats` is shared live with the caller (CLI/test metrics).
pub async fn run_tunnel(
    opts: RunOptions,
    role: Role,
    stop: Arc<AtomicBool>,
    on_event: Arc<dyn Fn(TunnelEvent) + Send + Sync>,
    stats: Arc<RuntimeStats>,
) -> Result<(Arc<RuntimeStats>, SocketAddr), io::Error> {
    let listener = TcpListener::bind(opts.listen).await?;
    let local_addr = listener.local_addr()?;
    on_event(TunnelEvent::Starting { role });
    on_event(TunnelEvent::Listening { addr: local_addr });

    let mut next_id: u64 = 1;
    let mut sessions: JoinSet<SessionReport> = JoinSet::new();

    loop {
        tokio::select! {
            _ = wait_for_stop(stop.clone()) => break,
            _ = tokio::time::sleep(Duration::from_millis(10)) => {}
            accepted = listener.accept() => {
                let (conn, peer_addr) = accepted?;
                let id = next_id;
                next_id += 1;
                stats.sessions_total.fetch_add(1, Ordering::SeqCst);
                on_event(TunnelEvent::Connected { id, peer: peer_addr });

                let dial_target = opts.peer;
                match dial_with_timeout(dial_target, opts.connect_timeout).await {
                    Ok(peer_socket) => {
                        let started = Instant::now();
                        let (tunnel, local) = match role {
                            Role::Host => (conn, peer_socket),
                            Role::Client => (peer_socket, conn),
                        };
                        sessions.spawn(handle_session(
                            tunnel,
                            local,
                            role,
                            opts.params,
                            opts.idle_timeout,
                            started,
                            stats.clone(),
                            id,
                        ));
                        on_event(TunnelEvent::Forwarding { id, peer: peer_addr });
                    }
                    Err(e) => {
                        stats.sessions_rejected.fetch_add(1, Ordering::SeqCst);
                        on_event(TunnelEvent::DialFailed {
                            id,
                            peer: dial_target,
                            error: e.to_string(),
                        });
                    }
                }
            }
        }
        prune_sessions(&mut sessions, &on_event);
    }

    drop(listener);
    // Abort in-flight sessions and drain the JoinSet so the runtime exits.
    sessions.abort_all();
    while let Some(result) = sessions.join_next().await {
        handle_report(result, &on_event);
    }
    on_event(TunnelEvent::Stopped);
    Ok((stats, local_addr))
}

fn handle_report(
    result: Result<SessionReport, tokio::task::JoinError>,
    on_event: &Arc<dyn Fn(TunnelEvent) + Send + Sync>,
) {
    if let Ok(report) = result {
        on_event(TunnelEvent::Disconnected {
            id: report.id,
            error: report.error.clone(),
        });
    }
}

fn prune_sessions(
    sessions: &mut JoinSet<SessionReport>,
    on_event: &Arc<dyn Fn(TunnelEvent) + Send + Sync>,
) {
    while let Some(result) = sessions.try_join_next() {
        handle_report(result, &on_event);
    }
}

async fn wait_for_stop(stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn dial_with_timeout(addr: SocketAddr, timeout: Duration) -> io::Result<TcpStream> {
    match tokio::time::timeout(timeout, TcpStream::connect(addr)).await {
        Ok(result) => result,
        Err(_) => Err(io::Error::new(io::ErrorKind::TimedOut, "connect timed out")),
    }
}

// ---------------------------------------------------------------------------
// E2E test harness (`voidlink test`)
// ---------------------------------------------------------------------------

/// One result line produced by the `voidlink test` harness.
#[derive(Debug, Clone)]
pub struct E2eLine {
    pub name: String,
    pub ok: bool,
    pub reason: Option<String>,
}

/// Aggregate result of a single `voidlink test` run.
#[derive(Debug)]
pub struct E2eSummary {
    pub lines: Vec<E2eLine>,
}

impl E2eSummary {
    pub fn tests(&self) -> usize {
        self.lines.len()
    }

    pub fn passed(&self) -> usize {
        self.lines.iter().filter(|l| l.ok).count()
    }

    pub fn failed(&self) -> usize {
        self.lines.iter().filter(|l| !l.ok).count()
    }

    pub fn passed_all(&self) -> bool {
        self.failed() == 0
    }
}

/// Plain TCP echo server: reads bytes on each accepted connection and writes
/// them straight back. Runs until `stop` is set, then closes the listener and
/// aborts any remaining echo sessions. The listener must already be bound by
/// the caller (so readiness is guaranteed); the bound address is returned.
///
/// This is deliberately a generic TCP endpoint, NOT a Minecraft server.
pub async fn run_echo_server(listener: TcpListener, stop: Arc<AtomicBool>) -> io::Result<SocketAddr> {
    let addr = listener.local_addr()?;
    let mut sessions = JoinSet::new();
    loop {
        tokio::select! {
            _ = wait_for_stop(stop.clone()) => break,
            accepted = listener.accept() => {
                let (stream, _) = match accepted {
                    Ok(x) => x,
                    Err(_) => continue,
                };
                sessions.spawn(async move {
                    let mut s = stream;
                    let mut buf = vec![0u8; CHUNK_SIZE];
                    loop {
                        match s.read(&mut buf).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                if s.write_all(&buf[..n]).await.is_err() {
                                    break;
                                }
                            }
                        }
                    }
                });
            }
        }
    }
    drop(listener);
    sessions.abort_all();
    while sessions.join_next().await.is_some() {}
    Ok(addr)
}

/// A running tunnel endpoint inside the E2E harness. Everything here is a real
/// TCP endpoint: its own listener, real sockets, the same `run_tunnel` engine
/// the CLI uses.
struct TunnelEp {
    task: JoinHandle<Result<(Arc<RuntimeStats>, SocketAddr), io::Error>>,
    stop: Arc<AtomicBool>,
    stats: Arc<RuntimeStats>,
    listen: SocketAddr,
    events: Arc<std::sync::Mutex<Vec<String>>>,
    role_name: String,
}

async fn start_tunnel_ep(
    role: Role,
    listen: SocketAddr,
    peer: SocketAddr,
    params: ProfileParams,
) -> Result<TunnelEp, String> {
    let (tx, mut rx) = mpsc::channel(256);
    let stop = Arc::new(AtomicBool::new(false));
    let stats = Arc::new(RuntimeStats::default());
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let events_cb = events.clone();
    let on_event: Arc<dyn Fn(TunnelEvent) + Send + Sync> = Arc::new(move |e: TunnelEvent| {
        let msg = match &e {
            TunnelEvent::Connected { id, peer } => format!("connected #{id} peer={peer}"),
            TunnelEvent::Forwarding { id, peer } => format!("forwarding #{id} peer={peer}"),
            TunnelEvent::DialFailed { id, peer, error } => {
                format!("dial failed #{id} peer={peer}: {error}")
            }
            TunnelEvent::Disconnected { id, error } => {
                let reason = error.as_deref().unwrap_or("eof");
                format!("disconnected #{id}: {reason}")
            }
            TunnelEvent::Listening { addr } => format!("listening on {addr}"),
            TunnelEvent::Starting { .. } => format!("starting"),
            TunnelEvent::Stopped => format!("stopped"),
        };
        events_cb.lock().unwrap().push(msg);
        let _ = tx.try_send(e);
    });
    let opts = RunOptions {
        listen,
        peer,
        params,
        connect_timeout: Duration::from_secs(5),
        idle_timeout: None,
    };
    let task = tokio::spawn(run_tunnel(opts, role, stop.clone(), on_event, stats.clone()));
    // Wait until the endpoint is really listening.
    let bound = loop {
        match tokio::time::timeout(Duration::from_secs(8), rx.recv()).await {
            Ok(Some(TunnelEvent::Listening { addr })) => break addr,
            Ok(Some(_)) => continue,
            Ok(None) => return Err(format!("{role} ended before it started listening")),
            Err(_) => return Err(format!("{role} did not start listening within 8 s")),
        }
    };
    Ok(TunnelEp {
        task,
        stop,
        stats,
        listen: bound,
        events,
        role_name: role.to_string(),
    })
}

fn e2e_payload(seed: u64, size: usize) -> Vec<u8> {
    let mut r = Rng::new(seed);
    let mut v = Vec::with_capacity(size);
    while v.len() < size {
        let w = r.next_u64();
        v.extend_from_slice(&w.to_le_bytes());
    }
    v.truncate(size);
    v
}

fn e2e_hash(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    hex::encode(h.finalize())
}

/// Read exactly `want` bytes (or stop at EOF/error/`timeout`). Returns a
/// precise description so a failing check shows how many bytes arrived.
async fn read_all_until<R>(s: &mut R, want: usize, timeout: Duration) -> Result<Vec<u8>, String>
where
    R: AsyncRead + Unpin,
{
    let mut got = Vec::with_capacity(want.min(1 << 20));
    let mut buf = vec![0u8; CHUNK_SIZE];
    while got.len() < want {
        match tokio::time::timeout(timeout, s.read(&mut buf)).await {
            Ok(Ok(0)) => {
                return Err(format!(
                    "connection closed after {}/{} bytes ({} bytes short)",
                    got.len(),
                    want,
                    want - got.len()
                ));
            }
            Ok(Ok(n)) => got.extend_from_slice(&buf[..n]),
            Ok(Err(e)) => {
                return Err(format!("read error after {}/{} bytes: {e}", got.len(), want));
            }
            Err(_) => {
                return Err(format!("timed out after {}/{} bytes", got.len(), want));
            }
        }
    }
    Ok(got)
}

/// Dial `addr`, write `data`, read the full echo back, verify byte-for-byte.
async fn e2e_echo(addr: SocketAddr, data: &[u8], timeout: Duration) -> Result<(), String> {
    let mut s = dial_with_timeout(addr, Duration::from_secs(5))
        .await
        .map_err(|e| format!("connect failed: {e}"))?;
    s.write_all(data)
        .await
        .map_err(|e| format!("write failed: {e}"))?;
    let got = read_all_until(&mut s, data.len(), timeout).await?;
    if got.len() != data.len() || got != data {
        let idx = got
            .iter()
            .zip(data.iter())
            .position(|(a, b)| a != b)
            .unwrap_or_else(|| got.len().min(data.len()));
        return Err(format!(
            "payload mismatch at byte {idx}: got {} bytes (sha256 {}), expected {}. Target: {addr}",
            got.len(),
            &e2e_hash(&got)[..16],
            data.len(),
        ));
    }
    let _ = s.shutdown().await;
    Ok(())
}

/// True bidirectional check: a reader task streams the echo back while the
/// writer task sends concurrently, verifying both directions at once.
async fn e2e_duplex(
    addr: SocketAddr,
    total: usize,
    chunk: usize,
    timeout: Duration,
) -> Result<(), String> {
    let s = dial_with_timeout(addr, Duration::from_secs(5))
        .await
        .map_err(|e| format!("connect failed: {e}"))?;
    let data = e2e_payload(1234, total);
    let want = e2e_hash(&data);
    let (mut rh, mut wh) = s.into_split();
    let reader = tokio::spawn(async move {
        let got = read_all_until(&mut rh, total, timeout).await?;
        let got_hash = e2e_hash(&got);
        if got_hash != want {
            return Err("duplex echo hash mismatch".to_string());
        }
        Ok::<(), String>(())
    });
    for part in data.chunks(chunk) {
        wh.write_all(part)
            .await
            .map_err(|e| format!("write failed while streaming: {e}"))?;
    }
    let _ = wh.shutdown().await;
    reader.await.map_err(|e| format!("reader task ended unexpectedly: {e}"))??;
    Ok(())
}

/// Multiple sequential, short-lived connections over the same tunnel.
async fn e2e_sequential(addr: SocketAddr, count: usize, timeout: Duration) -> Result<(), String> {
    for i in 0..count as u64 {
        let data = format!("seq-{i}").repeat(8).into_bytes();
        e2e_echo(addr, &data, timeout).await?;
    }
    Ok(())
}

/// `count` parallel echo sessions, each with per-session payload integrity.
async fn e2e_concurrent(
    addr: SocketAddr,
    count: usize,
    size: usize,
    timeout: Duration,
) -> Result<(), String> {
    let mut set = JoinSet::new();
    for i in 0..count as u64 {
        let a = addr;
        set.spawn(async move {
            let data = e2e_payload(5000 + i, size);
            e2e_echo(a, &data, timeout).await
        });
    }
    let mut failures = Vec::new();
    while let Some(r) = set.join_next().await {
        match r {
            Ok(Ok(())) => {}
            Ok(Err(e)) => failures.push(e),
            Err(e) => failures.push(format!("task panicked: {e}")),
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} of {} concurrent sessions failed; first error: {}",
            failures.len(),
            count,
            failures[0]
        ))
    }
}

/// Graceful-close propagation: after the echo completes, shut down the write
/// half, and expect the server side to observe EOF and close in return.
async fn e2e_graceful_close(addr: SocketAddr, timeout: Duration) -> Result<(), String> {
    let mut s = dial_with_timeout(addr, Duration::from_secs(5))
        .await
        .map_err(|e| format!("connect failed: {e}"))?;
    let data = b"close-me";
    s.write_all(data)
        .await
        .map_err(|e| format!("write failed: {e}"))?;
    let got = read_all_until(&mut s, data.len(), timeout).await?;
    if got != data {
        return Err(format!(
            "echo mismatch before close: got {} bytes",
            got.len()
        ));
    }
    // Close our write side: the echo server must see EOF, then close, and we
    // must observe a clean EOF back.
    let _ = s.shutdown().await;
    let mut buf = [0u8; 16];
    match tokio::time::timeout(timeout, s.read(&mut buf)).await {
        Ok(Ok(0)) => Ok(()),
        Ok(Ok(n)) => Err(format!("expected clean EOF, but {n} more bytes arrived")),
        Ok(Err(e)) => Err(format!("expected clean EOF, got a read error instead: {e}")),
        Err(_) => Err("clean EOF was never propagated within the timeout".to_string()),
    }
}

/// Reconnect: verify a *new* connection works right after a previous one was
/// closed cleanly.
async fn e2e_reconnect(addr: SocketAddr, timeout: Duration) -> Result<(), String> {
    e2e_echo(addr, b"first", timeout).await?;
    e2e_echo(addr, b"second", timeout).await
}

/// Error handling: dialing a port where nothing listens must produce an error
/// instead of crashing or hanging the test client.
async fn e2e_error_handling() -> Result<(), String> {
    let probe = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| format!("could not allocate a probe port: {e}"))?;
    let dead = probe.local_addr().map_err(|e| e.to_string())?;
    drop(probe);
    match dial_with_timeout(dead, Duration::from_secs(2)).await {
        Ok(_) => Err(format!("expected a dial to closed port {dead} to fail, but it connected")),
        Err(_) => Ok(()),
    }
}

/// After teardown, the harness must not leave any listener behind — rebinding
/// every component port must succeed.
async fn e2e_no_leftover(addrs: &[SocketAddr]) -> Result<(), String> {
    for a in addrs {
        match TcpListener::bind(*a).await {
            Ok(listener) => drop(listener),
            Err(e) => {
                return Err(format!("port {a} is still bound after teardown: {e}"));
            }
        }
    }
    Ok(())
}

/// A plain server that accepts connections, reads forever and never replies —
/// used to verify the test client surfaces *timeouts* (as opposed to EOF or
/// connect errors) instead of hanging.
async fn run_blackhole_server(listener: TcpListener, stop: Arc<AtomicBool>) -> io::Result<SocketAddr> {
    let addr = listener.local_addr()?;
    let mut sessions = JoinSet::new();
    loop {
        tokio::select! {
            _ = wait_for_stop(stop.clone()) => break,
            accepted = listener.accept() => {
                let (mut s, _) = match accepted {
                    Ok(x) => x,
                    Err(_) => continue,
                };
                sessions.spawn(async move {
                    let mut buf = vec![0u8; CHUNK_SIZE];
                    loop {
                        match s.read(&mut buf).await {
                            Ok(0) | Err(_) => break,
                            Ok(_) => {}
                        }
                    }
                });
            }
        }
    }
    drop(listener);
    sessions.abort_all();
    while sessions.join_next().await.is_some() {}
    Ok(addr)
}

/// Read-only view of one running harness, handed to suites. Carries the real
/// addresses plus the live stop handles and stats so abort/classification
/// scenarios can tear a component down mid-test and observe the result.
struct E2eCtx {
    profile: Profile,
    client_listen: SocketAddr,
    echo_stop: Arc<AtomicBool>,
    host_stop: Arc<AtomicBool>,
    client_stats: Arc<RuntimeStats>,
    host_stats: Arc<RuntimeStats>,
}

/// One E2E run: an embedded TCP echo server, a real VoidLink Host, a real
/// VoidLink Client and a shared result list. Suites run checks against it (via
/// `ctx`), then `teardown` stops every component, verifies nothing is left
/// listening and produces the summary.
struct Harness {
    profile: Profile,
    echo_addr: SocketAddr,
    echo_task: JoinHandle<io::Result<SocketAddr>>,
    echo_stop: Arc<AtomicBool>,
    client: TunnelEp,
    host: TunnelEp,
    lines: Vec<E2eLine>,
}

impl Harness {
    async fn start(profile: Profile, log: &dyn Fn(String)) -> Result<Harness, String> {
        log(format!("profile: {}", profile.name()));

        // 1. Embedded TCP test server (echo) — a real listener, not a fake.
        let echo_listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|e| format!("test server bind failed: {e}"))?;
        let echo_addr = echo_listener
            .local_addr()
            .map_err(|e| format!("test server local_addr failed: {e}"))?;
        let echo_stop = Arc::new(AtomicBool::new(false));
        let echo_task = tokio::spawn(run_echo_server(echo_listener, echo_stop.clone()));
        log(format!("[setup] test server listener on {echo_addr}"));

        // 2. VoidLink Host -> echo server.
        let host = start_tunnel_ep(
            Role::Host,
            "127.0.0.1:0".parse().unwrap(),
            echo_addr,
            profile.params(),
        )
        .await
        .map_err(|e| format!("voidlink host failed to start: {e}"))?;
        log(format!("[setup] voidlink host listener on {}", host.listen));

        // 3. VoidLink Client -> Host.
        let client = start_tunnel_ep(
            Role::Client,
            "127.0.0.1:0".parse().unwrap(),
            host.listen,
            profile.params(),
        )
        .await
        .map_err(|e| format!("voidlink client failed to start: {e}"))?;
        log(format!("[setup] voidlink client listener on {}", client.listen));

        Ok(Harness {
            profile,
            echo_addr,
            echo_task,
            echo_stop,
            client,
            host,
            lines: Vec::new(),
        })
    }

    fn ctx(&self) -> E2eCtx {
        E2eCtx {
            profile: self.profile,
            client_listen: self.client.listen,
            echo_stop: self.echo_stop.clone(),
            host_stop: self.host.stop.clone(),
            client_stats: self.client.stats.clone(),
            host_stats: self.host.stats.clone(),
        }
    }

    /// Stop every component, wait for the tasks, verify the ports are free, add
    /// a cleanup check and dump host/client diagnostics on failure so the report
    /// shows exactly where things went wrong (port, disconnect, byte/TCP error).
    async fn teardown(mut self, log: &dyn Fn(String)) -> E2eSummary {
        self.echo_stop.store(true, Ordering::SeqCst);
        self.client.stop.store(true, Ordering::SeqCst);
        self.host.stop.store(true, Ordering::SeqCst);
        let (echo_done, client_done, host_done) =
            tokio::join!(self.echo_task, &mut self.client.task, &mut self.host.task);
        if echo_done.is_err() {
            log("[teardown] test server task ended with a panic/abort".to_string());
        }
        if client_done.is_err() || host_done.is_err() {
            log("[teardown] a tunnel endpoint task ended with a panic/abort".to_string());
        }

        // Nothing may be left listening.
        record_check(
            &mut self.lines,
            "cleanup (no leftover ports)",
            e2e_no_leftover(&[self.echo_addr, self.client.listen, self.host.listen]),
        )
        .await;

        if !self.lines.iter().all(|l| l.ok) {
            for ep in [&self.client, &self.host] {
                log(format!(
                    "  [{}] bytes_upstream={} bytes_downstream={} sessions_total={} sessions_errored={}",
                    ep.role_name,
                    ep.stats.bytes_upstream.load(Ordering::SeqCst),
                    ep.stats.bytes_downstream.load(Ordering::SeqCst),
                    ep.stats.sessions_total.load(Ordering::SeqCst),
                    ep.stats.sessions_errored.load(Ordering::SeqCst),
                ));
                let evs = ep.events.lock().unwrap();
                if evs.is_empty() {
                    continue;
                }
                log(format!("[diag] voidlink {} last events:", ep.role_name));
                for m in evs.iter().rev().take(15) {
                    log(format!("  {m}"));
                }
            }
        }

        E2eSummary {
            lines: self.lines,
        }
    }
}

/// Poll `cond` until it is true or `timeout` elapses.
async fn wait_until(mut cond: impl FnMut() -> bool, timeout: Duration) -> bool {
    let start = tokio::time::Instant::now();
    while start.elapsed() < timeout {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    cond()
}

async fn e2e_sequential_sized(
    addr: SocketAddr,
    count: usize,
    size: usize,
    timeout: Duration,
) -> Result<(), String> {
    for i in 0..count as u64 {
        e2e_echo(addr, &e2e_payload(300 + i, size), timeout).await?;
    }
    Ok(())
}

/// Multiple connect -> transfer -> close -> reconnect cycles over one tunnel.
async fn e2e_cycles(addr: SocketAddr, cycles: usize, timeout: Duration) -> Result<(), String> {
    for i in 0..cycles as u64 {
        let data = format!("cycle-{i}").repeat(24).into_bytes();
        e2e_echo(addr, &data, timeout).await?;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Ok(())
}

/// Fuzz-style check: random sizes and random contents, verified byte-exact
/// (the failure message also carries the SHA-256 of the received bytes).
async fn e2e_fuzz(
    addr: SocketAddr,
    iterations: usize,
    max_size: usize,
    timeout: Duration,
) -> Result<(), String> {
    let mut rng = Rng::new(time_seed() ^ 0xF0F0_F0F0_0000_0001);
    let mut failures: Vec<String> = Vec::new();
    for _ in 0..iterations {
        let size = 1 + rng.next_below(max_size as u64) as usize;
        let seed = rng.next_u64();
        let data = e2e_payload(seed, size);
        if let Err(e) = e2e_echo(addr, &data, timeout).await {
            failures.push(format!("size={size} seed={seed:#x}: {e}"));
            if failures.len() >= 3 {
                break;
            }
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} fuzz cases failed; first: {}",
            failures.len(),
            failures[0]
        ))
    }
}

/// A remote that accepts one connection and closes after one byte: the test
/// client must classify this as a clean EOF (normal close), not an error.
async fn e2e_remote_eof() -> Result<(), String> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| format!("bind failed: {e}"))?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;
    let handle = tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        let mut b = [0u8; 1];
        let _ = s.read(&mut b).await;
        drop(s);
    });
    let mut s = dial_with_timeout(addr, Duration::from_secs(5))
        .await
        .map_err(|e| format!("connect failed: {e}"))?;
    s.write_all(b"x")
        .await
        .map_err(|e| format!("write failed: {e}"))?;
    let mut buf = [0u8; 8];
    match tokio::time::timeout(Duration::from_secs(5), s.read(&mut buf)).await {
        Ok(Ok(0)) => {}
        Ok(Ok(n)) => return Err(format!("expected clean EOF, but {n} bytes arrived")),
        Ok(Err(e)) => return Err(format!("expected clean EOF, got a read error instead: {e}")),
        Err(_) => return Err("expected a clean EOF, but the read timed out".to_string()),
    }
    handle.await.map_err(|e| format!("server task panicked: {e}"))?;
    Ok(())
}

/// Abrupt client close: the test client tears the connection down with no
/// graceful shutdown (and never reads the pending echo), so the peer observes a
/// torn-down stream rather than a clean FIN. The tunnel must end that session
/// and keep serving new connections. (Stable Rust exposes no SO_LINGER, so a
/// guaranteed RST is not available; the assertion is "session ended + recovery".)
async fn e2e_abrupt_client_close(ctx: &E2eCtx, timeout: Duration) -> Result<(), String> {
    let mut s = dial_with_timeout(ctx.client_listen, Duration::from_secs(5))
        .await
        .map_err(|e| format!("connect failed: {e}"))?;
    s.write_all(b"abort-me")
        .await
        .map_err(|e| format!("write failed: {e}"))?;
    drop(s); // ungraceful close, no shutdown, echo left unread

    // The aborted session must be torn down rather than left hanging.
    let stats = ctx.client_stats.clone();
    if !wait_until(
        move || stats.active.load(Ordering::SeqCst) == 0,
        timeout,
    )
    .await
    {
        return Err(format!(
            "aborted session was never torn down (client still has active sessions after {} s)",
            timeout.as_secs()
        ));
    }

    // The tunnel recovers: a fresh connection works normally.
    e2e_echo(ctx.client_listen, b"post-abort", timeout).await?;
    Ok(())
}

/// Stop the Host mid-stream; the test client must observe the break as an EOF
/// or a reset (never a hang), proving an abrupt component death is surfaced.
async fn e2e_host_shutdown(ctx: &E2eCtx, timeout: Duration) -> Result<(), String> {
    let mut s = dial_with_timeout(ctx.client_listen, Duration::from_secs(5))
        .await
        .map_err(|e| format!("connect failed: {e}"))?;
    s.write_all(&e2e_payload(21, 64 * 1024))
        .await
        .map_err(|e| format!("write failed: {e}"))?;

    let mut buf = [0u8; 1024];
    match tokio::time::timeout(Duration::from_secs(10), s.read(&mut buf)).await {
        Ok(Ok(n)) if n > 0 => {}
        Ok(Ok(_)) => return Err("echo closed before host shutdown".to_string()),
        Ok(Err(e)) => return Err(format!("read error before host shutdown: {e}")),
        Err(_) => return Err("tunnel did not start echoing within the test window".to_string()),
    }

    ctx.host_stop.store(true, Ordering::SeqCst);

    let mut saw_close = false;
    let start = tokio::time::Instant::now();
    while start.elapsed() < timeout {
        match tokio::time::timeout(Duration::from_millis(800), s.read(&mut buf)).await {
            Ok(Ok(0)) | Ok(Err(_)) => {
                saw_close = true;
                break;
            }
            Ok(Ok(_)) => {}
            Err(_) => {}
        }
    }
    if !saw_close {
        return Err("test client did not observe the host shutdown (no EOF/reset)".to_string());
    }
    Ok(())
}

/// Stop the echo server mid-stream; the test client must see the break while
/// the Host keeps running (the "server closes under us" scenario).
async fn e2e_echo_stop_mid(ctx: &E2eCtx, timeout: Duration) -> Result<(), String> {
    let mut s = dial_with_timeout(ctx.client_listen, Duration::from_secs(5))
        .await
        .map_err(|e| format!("connect failed: {e}"))?;
    s.write_all(&e2e_payload(22, 64 * 1024))
        .await
        .map_err(|e| format!("write failed: {e}"))?;

    let mut buf = [0u8; 1024];
    match tokio::time::timeout(Duration::from_secs(10), s.read(&mut buf)).await {
        Ok(Ok(n)) if n > 0 => {}
        Ok(Ok(_)) => return Err("echo closed before the shutdown test".to_string()),
        Ok(Err(e)) => return Err(format!("read error before echo shutdown: {e}")),
        Err(_) => return Err("tunnel did not start echoing within the test window".to_string()),
    }

    ctx.echo_stop.store(true, Ordering::SeqCst);

    let mut saw_close = false;
    let start = tokio::time::Instant::now();
    while start.elapsed() < timeout {
        match tokio::time::timeout(Duration::from_millis(800), s.read(&mut buf)).await {
            Ok(Ok(0)) | Ok(Err(_)) => {
                saw_close = true;
                break;
            }
            Ok(Ok(_)) => {}
            Err(_) => {}
        }
    }
    if !saw_close {
        return Err("test client did not observe the echo shutdown (no EOF/reset)".to_string());
    }
    Ok(())
}

/// After the echo server is dead, a fresh session must be surfaced as a
/// rejected dial on the Host (the "connect failed" path) — the client stays up.
async fn e2e_dial_refused(ctx: &E2eCtx, timeout: Duration) -> Result<(), String> {
    let started = tokio::time::Instant::now();
    let mut attempts = 0u32;
    loop {
        attempts += 1;
        if let Ok(mut s) = dial_with_timeout(ctx.client_listen, Duration::from_secs(2)).await {
            let _ = s.write_all(b"x").await;
        }
        let stats = ctx.host_stats.clone();
        if wait_until(
            move || stats.sessions_rejected.load(Ordering::SeqCst) >= 1,
            Duration::from_millis(500),
        )
        .await
        {
            return Ok(());
        }
        if started.elapsed() >= timeout {
            return Err(format!(
                "host did not surface a rejected dial after the echo server died ({attempts} attempts)"
            ));
        }
    }
}

/// A peer that accepts but never responds must surface as a read *timeout* in
/// the test client — not as an EOF and not as a hang. Uses its own blackhole
/// server plus a dedicated Host/Client pair, then verifies cleanup.
async fn e2e_timeout_surfaced(timeout: Duration) -> Result<(), String> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| format!("blackhole bind failed: {e}"))?;
    let bh_addr = listener
        .local_addr()
        .map_err(|e| format!("blackhole local_addr failed: {e}"))?;
    let stop = Arc::new(AtomicBool::new(false));
    let bh_task = tokio::spawn(run_blackhole_server(listener, stop.clone()));

    let zero = ProfileParams::zero();
    let mut bh_host = start_tunnel_ep(Role::Host, "127.0.0.1:0".parse().unwrap(), bh_addr, zero)
        .await
        .map_err(|e| format!("timeout-host failed to start: {e}"))?;
    let mut bh_client =
        start_tunnel_ep(Role::Client, "127.0.0.1:0".parse().unwrap(), bh_host.listen, zero)
            .await
            .map_err(|e| format!("timeout-client failed to start: {e}"))?;

    let mut s = dial_with_timeout(bh_client.listen, Duration::from_secs(5))
        .await
        .map_err(|e| format!("connect failed: {e}"))?;
    s.write_all(b"ping")
        .await
        .map_err(|e| format!("write failed: {e}"))?;
    let outcome = read_all_until(&mut s, 64, timeout).await;
    drop(s);

    stop.store(true, Ordering::SeqCst);
    bh_client.stop.store(true, Ordering::SeqCst);
    bh_host.stop.store(true, Ordering::SeqCst);
    let (bh_done, c_done, h_done) = tokio::join!(bh_task, &mut bh_client.task, &mut bh_host.task);
    if bh_done.is_err() || c_done.is_err() || h_done.is_err() {
        return Err("timeout-test teardown: a component task ended with a panic/abort".to_string());
    }
    e2e_no_leftover(&[bh_addr, bh_client.listen, bh_host.listen]).await?;

    match outcome {
        Err(e) if e.contains("timed out") => Ok(()),
        Err(e) => Err(format!("expected a timeout, but got a different failure: {e}")),
        Ok(data) => Err(format!(
            "expected a timeout, but the silent peer answered {} bytes",
            data.len()
        )),
    }
}

/// Extended load checks on the normal profile (the `--all` load section):
/// multi-MiB payloads, transfer cycles and higher concurrency.
async fn run_heavy_suite(ctx: &E2eCtx, lines: &mut Vec<E2eLine>) {
    let addr = ctx.client_listen;
    let tiny = Duration::from_secs(15);
    record_check(
        lines,
        "single 1 MiB payload",
        e2e_echo(addr, &e2e_payload(11, 1024 * 1024), Duration::from_secs(120)),
    )
    .await;
    record_check(
        lines,
        "multi-MiB: 8 MiB payload",
        e2e_echo(addr, &e2e_payload(12, 8 * 1024 * 1024), Duration::from_secs(120)),
    )
    .await;
    record_check(
        lines,
        "sequential transfers (4 × 2 MiB)",
        e2e_sequential_sized(addr, 4, 2 * 1024 * 1024, Duration::from_secs(120)),
    )
    .await;
    record_check(lines, "connect/transfer/close/reconnect cycles (20)", e2e_cycles(addr, 20, tiny)).await;
    record_check(
        lines,
        "concurrent sessions (50)",
        e2e_concurrent(addr, 50, 16 * 1024, Duration::from_secs(60)),
    )
    .await;
    record_check(
        lines,
        "concurrent sessions (100)",
        e2e_concurrent(addr, 100, 16 * 1024, Duration::from_secs(60)),
    )
    .await;
    record_check(
        lines,
        "bidirectional duplex (1 MiB)",
        e2e_duplex(addr, 1024 * 1024, 16 * 1024, Duration::from_secs(60)),
    )
    .await;
    record_check(lines, "recovery echo after heavy load", e2e_echo(addr, b"done-load", tiny)).await;
}

/// Fuzz-like random payload checks on the normal profile.
async fn run_fuzz_suite(ctx: &E2eCtx, lines: &mut Vec<E2eLine>) {
    let addr = ctx.client_listen;
    record_check(
        lines,
        "32 random payloads (1 B..256 KiB, SHA-256 + exact bytes)",
        e2e_fuzz(addr, 32, 256 * 1024, Duration::from_secs(30)),
    )
    .await;
    record_check(
        lines,
        "8 random payloads (1 B..1 MiB)",
        e2e_fuzz(addr, 8, 1024 * 1024, Duration::from_secs(90)),
    )
    .await;
}

async fn suite_abrupt_client_close(ctx: &E2eCtx, lines: &mut Vec<E2eLine>) {
    record_check(
        lines,
        "small echo before abort",
        e2e_echo(ctx.client_listen, b"pre", Duration::from_secs(10)),
    )
    .await;
    record_check(
        lines,
        "abrupt client close (ungraceful, no clean EOF) mid-transfer",
        e2e_abrupt_client_close(ctx, Duration::from_secs(10)),
    )
    .await;
    record_check(
        lines,
        "reconnect after abort",
        e2e_echo(ctx.client_listen, b"post", Duration::from_secs(10)),
    )
    .await;
}

async fn suite_host_shutdown(ctx: &E2eCtx, lines: &mut Vec<E2eLine>) {
    record_check(
        lines,
        "host shutdown mid-transfer (EOF/reset surfaced)",
        e2e_host_shutdown(ctx, Duration::from_secs(20)),
    )
    .await;
}

async fn suite_echo_shutdown(ctx: &E2eCtx, lines: &mut Vec<E2eLine>) {
    record_check(
        lines,
        "echo server shutdown mid-transfer (EOF/reset surfaced)",
        e2e_echo_stop_mid(ctx, Duration::from_secs(20)),
    )
    .await;
    record_check(
        lines,
        "dial refused after server death",
        e2e_dial_refused(ctx, Duration::from_secs(5)),
    )
    .await;
}

/// The three distinct failure classifications a test client can observe:
/// normal close (clean EOF), timeout (silent peer) and connect error.
async fn suite_error_classification(_ctx: &E2eCtx, lines: &mut Vec<E2eLine>) {
    record_check(lines, "clean remote close -> EOF (normal close)", e2e_remote_eof()).await;
    record_check(
        lines,
        "timeout surfaced (silent peer, not a hang)",
        e2e_timeout_surfaced(Duration::from_millis(1500)),
    )
    .await;
    record_check(lines, "connect error (refused dial)", e2e_error_handling()).await;
}

/// Standard 12-check suite for the stable profiles. The same harness runs
/// under every profile; only payload sizes scale down on slow profiles.
async fn run_standard_suite(ctx: &E2eCtx, lines: &mut Vec<E2eLine>) {
    let addr = ctx.client_listen;
    let profile = ctx.profile;
    // (medium, large, per-session concurrency size, duplex total, duplex chunk)
    let (medium, large, conc, dup_total, dup_chunk) = match profile {
        Profile::Normal => (64 * 1024, 1024 * 1024, 64 * 1024, 256 * 1024, 16 * 1024),
        Profile::HighLatency => (64 * 1024, 256 * 1024, 4 * 1024, 64 * 1024, 8 * 1024),
        Profile::Lossy => (64 * 1024, 256 * 1024, 4 * 1024, 64 * 1024, 8 * 1024),
        Profile::VeryBad => (16 * 1024, 64 * 1024, 2 * 1024, 32 * 1024, 4 * 1024),
        Profile::Disconnect => unreachable!("disconnect uses its own suite"),
    };

    let tiny = Duration::from_secs(10);
    let medium_t = Duration::from_secs(30);
    let large_t = Duration::from_secs(60);
    let dup_t = Duration::from_secs(30);

    record_check(lines, "small payload (11 B)", e2e_echo(addr, b"hello world", tiny)).await;
    record_check(lines, "minimal payload (1 B)", e2e_echo(addr, &[0xAB], tiny)).await;
    record_check(
        lines,
        "1 KiB payload",
        e2e_echo(addr, &e2e_payload(1, 1024), tiny),
    )
    .await;
    record_check(
        lines,
        &format!("medium payload ({} KiB)", medium / 1024),
        e2e_echo(addr, &e2e_payload(2, medium), medium_t),
    )
    .await;
    record_check(
        lines,
        &format!(
            "large payload ({})",
            if large >= 1024 * 1024 {
                "1 MiB".to_string()
            } else {
                format!("{} KiB", large / 1024)
            }
        ),
        e2e_echo(addr, &e2e_payload(3, large), large_t),
    )
    .await;
    record_check(
        lines,
        "bidirectional duplex",
        e2e_duplex(addr, dup_total, dup_chunk, dup_t),
    )
    .await;
    record_check(
        lines,
        "sequential sessions (10)",
        e2e_sequential(addr, 10, tiny),
    )
    .await;
    record_check(
        lines,
        "concurrent sessions (25)",
        e2e_concurrent(addr, 25, conc, tiny),
    )
    .await;
    record_check(lines, "graceful close (EOF propagation)", e2e_graceful_close(addr, tiny)).await;
    record_check(lines, "reconnect after close", e2e_reconnect(addr, tiny)).await;
    record_check(lines, "error handling (refused dial)", e2e_error_handling()).await;
}

/// Special suite for `--profile disconnect`: the link drops once 4 s after a
/// session starts, which would break long transfers on purpose. Instead verify
/// a healthy session first, then that the outage is surfaced, then recovery.
async fn run_disconnect_suite(ctx: &E2eCtx, lines: &mut Vec<E2eLine>) {
    let t = Duration::from_secs(10);
    record_check(
        lines,
        "small payload before outage",
        e2e_echo(ctx.client_listen, b"pre-outage", t),
    )
    .await;
    record_check(lines, "outage drops active session", e2e_outage_drops(ctx.client_listen, Duration::from_secs(15))).await;
    record_check(lines, "reconnect after outage", e2e_echo(ctx.client_listen, b"post-outage", t)).await;
    record_check(lines, "error handling (refused dial)", e2e_error_handling()).await;
}

/// Medium-sized transfer for a stable profile's short suite (scaled down on
/// slow profiles so `--all` stays quick).
fn profile_medium_bytes(profile: Profile) -> usize {
    match profile {
        Profile::Normal | Profile::HighLatency | Profile::Lossy => 32 * 1024,
        Profile::VeryBad => 16 * 1024,
        Profile::Disconnect => unreachable!("disconnect uses its own suite"),
    }
}

/// Per-profile section used by `--all`: a quick basic E2E + transfer +
/// reconnect/error pass under every profile (disconnect uses its recovery suite).
async fn run_profile_suite(ctx: &E2eCtx, lines: &mut Vec<E2eLine>) {
    if ctx.profile == Profile::Disconnect {
        run_disconnect_suite(ctx, lines).await;
        return;
    }
    let medium = profile_medium_bytes(ctx.profile);
    record_check(
        lines,
        "1 KiB payload",
        e2e_echo(ctx.client_listen, &e2e_payload(10, 1024), Duration::from_secs(15)),
    )
    .await;
    record_check(
        lines,
        &format!("medium transfer ({} KiB)", medium / 1024),
        e2e_echo(ctx.client_listen, &e2e_payload(20, medium), Duration::from_secs(60)),
    )
    .await;
    record_check(
        lines,
        "reconnect",
        e2e_reconnect(ctx.client_listen, Duration::from_secs(15)),
    )
    .await;
    record_check(lines, "error handling (refused dial)", e2e_error_handling()).await;
}

/// Keep pushing heartbeat bytes on one session while `disconnect`'s 4 s outage
/// fires; the session must be closed by the simulator and surfaced to the test
/// client as EOF or a read/write error.
async fn e2e_outage_drops(addr: SocketAddr, timeout: Duration) -> Result<(), String> {
    let mut s = dial_with_timeout(addr, Duration::from_secs(5))
        .await
        .map_err(|e| format!("connect failed: {e}"))?;
    let started = tokio::time::Instant::now();
    let mut buf = [0u8; 8];
    let mut saw_close = false;
    while started.elapsed() < timeout {
        let write_res = s.write_all(b"hb01").await;
        if write_res.is_err() {
            saw_close = true;
            break;
        }
        tokio::select! {
            r = s.read(&mut buf) => match r {
                Ok(0) | Err(_) => { saw_close = true; break; }
                Ok(_) => {}
            },
            _ = tokio::time::sleep(Duration::from_millis(80)) => {}
        }
    }
    if saw_close {
        Ok(())
    } else {
        Err(format!(
            "the simulated outage did not close the session within {} s",
            timeout.as_secs()
        ))
    }
}

async fn record_check<F>(lines: &mut Vec<E2eLine>, name: &str, fut: F)
where
    F: Future<Output = Result<(), String>>,
{
    match fut.await {
        Ok(()) => lines.push(E2eLine {
            name: name.to_string(),
            ok: true,
            reason: None,
        }),
        Err(reason) => lines.push(E2eLine {
            name: name.to_string(),
            ok: false,
            reason: Some(reason),
        }),
    }
}

/// Full end-to-end pipeline for `voidlink test`: an embedded TCP echo server,
/// a real VoidLink Host, a real VoidLink Client, then a test client dialing the
/// Client's listen socket. All four components are real TCP endpoints; setup,
/// checks and teardown are all driven from this one function. On any component
/// startup failure a descriptive `Err` is returned; on overall failure the
/// summary already contains per-check reasons.
pub async fn run_e2e_test(profile: Profile, log: &dyn Fn(String)) -> Result<E2eSummary, String> {
    let mut h = Harness::start(profile, log).await?;
    log(format!("[setup] test client dials {}", h.client.listen));
    match profile {
        Profile::Disconnect => run_disconnect_suite(&h.ctx(), &mut h.lines).await,
        _ => run_standard_suite(&h.ctx(), &mut h.lines).await,
    }
    Ok(h.teardown(log).await)
}

// ---------------------------------------------------------------------------
// Aggregated modes: `--repeat` and the full `--all` matrix
// ---------------------------------------------------------------------------

/// Outcome of `run_repeat_test`: how many full runs happened and which ones
/// (run index, 1-based) failed, with their complete check lists.
pub struct RepeatSummary {
    pub runs: usize,
    pub passed: usize,
    pub failed: usize,
    pub failed_details: Vec<(usize, Vec<E2eLine>)>,
}

/// Runs the standard E2E pipeline `repeat` times, each run with completely
/// fresh endpoints (create -> test -> destroy -> next). Quiet on success;
/// on a failed run its whole log is re-emitted so diagnostics stay available.
pub async fn run_repeat_test(
    profile: Profile,
    repeat: u32,
    log: &dyn Fn(String),
) -> Result<RepeatSummary, String> {
    let mut passed = 0usize;
    let mut failed_details = Vec::new();
    for i in 1..=repeat {
        let run_logs = std::cell::RefCell::new(Vec::<String>::new());
        let summary = run_e2e_test(profile, &|m| run_logs.borrow_mut().push(m.to_string())).await?;
        let passed_checks = summary.passed();
        let total_checks = summary.tests();
        if summary.passed_all() {
            passed += 1;
            log(format!("run {i}/{repeat}: {passed_checks}/{total_checks} checks PASS"));
        } else {
            failed_details.push((i as usize, summary.lines));
            log(format!("run {i}/{repeat}: {passed_checks}/{total_checks} checks FAIL"));
            for m in run_logs.borrow().iter() {
                log(format!("  {m}"));
            }
        }
    }
    let runs = repeat as usize;
    Ok(RepeatSummary {
        runs,
        passed,
        failed: runs - passed,
        failed_details,
    })
}

/// One titled group of checks within the full `--all` report.
pub struct SectionResult {
    pub title: String,
    pub lines: Vec<E2eLine>,
    pub note: Option<String>,
}

/// Aggregate result of `run_full_test`.
pub struct FullTestSummary {
    pub sections: Vec<SectionResult>,
}

impl FullTestSummary {
    pub fn tests(&self) -> usize {
        self.sections.iter().map(|s| s.lines.len()).sum()
    }

    pub fn passed(&self) -> usize {
        self.sections
            .iter()
            .flat_map(|s| s.lines.iter())
            .filter(|l| l.ok)
            .count()
    }

    pub fn failed(&self) -> usize {
        self.tests() - self.passed()
    }

    pub fn passed_all(&self) -> bool {
        self.failed() == 0
    }
}

/// The full `voidlink test --all` matrix: the standard E2E pipeline plus the
/// heavier load, fuzz, abort and error-classification sections, every profile,
/// and a repeat sweep. Every section runs in its own harness with fully fresh
/// endpoints and a cleanup check, then is torn down.
pub async fn run_full_test(repeat: u32, log: &dyn Fn(String)) -> Result<FullTestSummary, String> {
    let mut sections = Vec::new();

    log("[section] basic E2E (normal profile)".to_string());
    let mut h = Harness::start(Profile::Normal, log).await?;
    run_standard_suite(&h.ctx(), &mut h.lines).await;
    sections.push(SectionResult {
        title: "Basic E2E (normal profile)".to_string(),
        lines: h.teardown(log).await.lines,
        note: None,
    });

    log("[section] heavier load (multi-MiB, 50/100 concurrent, cycles)".to_string());
    let mut h = Harness::start(Profile::Normal, log).await?;
    run_heavy_suite(&h.ctx(), &mut h.lines).await;
    sections.push(SectionResult {
        title: "Heavier load (normal profile)".to_string(),
        lines: h.teardown(log).await.lines,
        note: None,
    });

    log("[section] fuzz — random sizes and contents, byte-exact".to_string());
    let mut h = Harness::start(Profile::Normal, log).await?;
    run_fuzz_suite(&h.ctx(), &mut h.lines).await;
    sections.push(SectionResult {
        title: "Fuzz payloads (random)".to_string(),
        lines: h.teardown(log).await.lines,
        note: None,
    });

    log("[section] abort & error scenarios".to_string());
    let mut h = Harness::start(Profile::Normal, log).await?;
    suite_abrupt_client_close(&h.ctx(), &mut h.lines).await;
    sections.push(SectionResult {
        title: "Abort: abnormal client close".to_string(),
        lines: h.teardown(log).await.lines,
        note: None,
    });

    let mut h = Harness::start(Profile::Normal, log).await?;
    suite_host_shutdown(&h.ctx(), &mut h.lines).await;
    sections.push(SectionResult {
        title: "Abort: host shutdown mid-transfer".to_string(),
        lines: h.teardown(log).await.lines,
        note: None,
    });

    let mut h = Harness::start(Profile::Normal, log).await?;
    suite_echo_shutdown(&h.ctx(), &mut h.lines).await;
    sections.push(SectionResult {
        title: "Abort: echo server shutdown mid-transfer".to_string(),
        lines: h.teardown(log).await.lines,
        note: None,
    });

    let mut h = Harness::start(Profile::Normal, log).await?;
    suite_error_classification(&h.ctx(), &mut h.lines).await;
    sections.push(SectionResult {
        title: "Error classification (EOF / timeout / refused)".to_string(),
        lines: h.teardown(log).await.lines,
        note: None,
    });

    for p in [
        Profile::HighLatency,
        Profile::Lossy,
        Profile::VeryBad,
        Profile::Disconnect,
    ] {
        log(format!("[section] profile {}", p.name()));
        let mut h = Harness::start(p, log).await?;
        run_profile_suite(&h.ctx(), &mut h.lines).await;
        sections.push(SectionResult {
            title: format!("Profile {}", p.name()),
            lines: h.teardown(log).await.lines,
            note: None,
        });
    }

    log(format!("[section] repeat sweep ({repeat} runs, fresh endpoints each)"));
    let mut rep_lines = Vec::new();
    let mut rep_passed = 0usize;
    let mut rep_failed = 0usize;
    for i in 1..=repeat {
        let mut h = Harness::start(Profile::Normal, log).await?;
        run_standard_suite(&h.ctx(), &mut h.lines).await;
        let s = h.teardown(log).await;
        let name = format!("repeat run {i}/{repeat}");
        if s.passed_all() {
            rep_passed += 1;
            rep_lines.push(E2eLine {
                name,
                ok: true,
                reason: None,
            });
        } else {
            rep_failed += 1;
            rep_lines.push(E2eLine {
                name,
                ok: false,
                reason: Some(format!("{} of {} checks failed", s.failed(), s.tests())),
            });
        }
    }
    sections.push(SectionResult {
        title: "Repeat (fresh endpoints per run)".to_string(),
        lines: rep_lines,
        note: Some(format!("Runs: {repeat} / Passed: {rep_passed} / Failed: {rep_failed}")),
    });

    Ok(FullTestSummary { sections })
}

// ---------------------------------------------------------------------------
// CLI parsing (shared by the binary; tested here)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Host,
    Client,
    /// One-shot self-test: embedded echo server + Host + Client + test client.
    Test,
    /// Run only the embedded echo TCP endpoint.
    TestServer,
}

#[derive(Debug)]
pub struct CliConfig {
    pub command: Command,
    pub listen: SocketAddr,
    pub peer: SocketAddr,
    pub params: ProfileParams,
    pub profile: Profile,
    pub connect_timeout: Duration,
    pub idle_timeout: Option<Duration>,
    /// `--all`: run the full test matrix (valid only with `test`).
    pub full_all: bool,
    /// `--repeat <n>`: how many times to repeat the test (>= 1).
    pub repeat: u32,
    /// Whether `--repeat` was explicitly given (callers use a default for
    /// `--all` when it wasn't).
    pub repeat_set: bool,
}

#[derive(Debug)]
pub enum ParseOutcome {
    Run(CliConfig),
    Help,
    Version,
}

/// Validates that a listener/peer address only ever binds a loopback
/// interface — local tunnelling must never open external ports.
fn parse_loopback_addr(raw: &str) -> Result<SocketAddr, String> {
    let addr: SocketAddr = raw
        .parse()
        .map_err(|_| format!("invalid address '{raw}': expected host:port"))?;
    if !addr.ip().is_loopback() {
        return Err(format!(
            "address '{raw}' is not a loopback address; VoidLink only binds 127.0.0.1/::1"
        ));
    }
    Ok(addr)
}

/// Parses the CLI (subcommand + optional flags). Loopback-only by design.
pub fn parse_cli<I>(args: I) -> Result<ParseOutcome, String>
where
    I: IntoIterator<Item = String>,
{
    let mut command: Option<Command> = None;
    let mut listen: Option<String> = None;
    let mut peer: Option<String> = None;
    let mut profile = Profile::Normal;
    let mut profile_set = false;
    let mut all = false;
    let mut repeat: Option<u64> = None;
    let mut overrides: Vec<(&'static str, u64)> = Vec::new();
    let mut connect_timeout_secs: Option<u64> = None;
    let mut idle_timeout_secs: Option<u64> = None;

    let mut args = args.into_iter().peekable();

    while let Some(arg) = args.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) => (f.to_string(), Some(v.to_string())),
            None => (arg.clone(), None),
        };
        // Strip leading dashes: "--listen" -> "listen", "-h" -> "h"
        let flag_clean = flag.trim_start_matches('-');
        let take_value = |next: Option<String>| -> Result<String, String> {
            if let Some(v) = inline.clone() {
                return Ok(v);
            }
            next.ok_or_else(|| format!("flag '{flag}' requires a value"))
        };

        match flag_clean {
            "help" | "-h" | "--help" => return Ok(ParseOutcome::Help),
            "version" | "-V" | "--version" => return Ok(ParseOutcome::Version),
            "host" => {
                if command.replace(Command::Host).is_some() {
                    return Err("command specified twice".to_string());
                }
            }
            "client" => {
                if command.replace(Command::Client).is_some() {
                    return Err("command specified twice".to_string());
                }
            }
            "test" => {
                if command.replace(Command::Test).is_some() {
                    return Err("command specified twice".to_string());
                }
            }
            "test-server" => {
                if command.replace(Command::TestServer).is_some() {
                    return Err("command specified twice".to_string());
                }
            }
            "listen" => listen = Some(take_value(args.next())?),
            "server" => peer = Some(take_value(args.next())?),
            "host-addr" => peer = Some(take_value(args.next())?),
            "profile" => {
                let name = take_value(args.next())?;
                profile = Profile::from_name(&name)
                    .ok_or_else(|| format!("unknown profile '{name}'"))?;
                profile_set = true;
            }
            "all" => all = true,
            "repeat" => repeat = Some(parse_u64(take_value(args.next())?)?),
            "latency-ms" => overrides.push(("latency-ms", parse_u64(take_value(args.next())?)?)),
            "jitter-ms" => overrides.push(("jitter-ms", parse_u64(take_value(args.next())?)?)),
            "loss-percent" => overrides.push(("loss-percent", parse_u64(take_value(args.next())?)?)),
            "loss-stall-ms" => overrides.push(("loss-stall-ms", parse_u64(take_value(args.next())?)?)),
            "drop-percent" => overrides.push(("drop-percent", parse_u64(take_value(args.next())?)?)),
            "bandwidth-kbps" => overrides.push(("bandwidth-kbps", parse_u64(take_value(args.next())?)?)),
            "outage-after-ms" => overrides.push(("outage-after-ms", parse_u64(take_value(args.next())?)?)),
            "connect-timeout-secs" => connect_timeout_secs = Some(parse_u64(take_value(args.next())?)?),
            "idle-timeout-secs" => idle_timeout_secs = Some(parse_u64(take_value(args.next())?)?),
            other => return Err(format!("unknown argument '{other}'")),
        }
    }

    let command =
        command.ok_or_else(|| "missing command: use 'host', 'client', 'test' or 'test-server'".to_string())?;

    if all && command != Command::Test {
        return Err("--all is only supported with the 'test' command".to_string());
    }
    if repeat.is_some() && command != Command::Test {
        return Err("--repeat is only supported with the 'test' command".to_string());
    }
    if let Some(r) = repeat {
        if r == 0 {
            return Err("--repeat must be at least 1".to_string());
        }
    }
    if all && profile_set {
        return Err(
            "--profile cannot be combined with --all (the full test already runs every profile)"
                .to_string(),
        );
    }

    let default_listen = match command {
        Command::Host => DEFAULT_HOST_LISTEN,
        Command::Client => DEFAULT_CLIENT_LISTEN,
        Command::Test => "127.0.0.1:0",
        Command::TestServer => DEFAULT_TEST_SERVER_LISTEN,
    };
    let listen_addr = parse_loopback_addr(listen.as_deref().unwrap_or(default_listen))?;

    let default_peer = match command {
        Command::Host => DEFAULT_SERVER_ADDR,
        Command::Client => DEFAULT_HOST_LISTEN,
        Command::Test | Command::TestServer => DEFAULT_SERVER_ADDR,
    };
    let peer_addr = parse_loopback_addr(peer.as_deref().unwrap_or(default_peer))?;

    let params = ProfileParams::from_profile_with_overrides(profile, &overrides)?;
    let connect_timeout = Duration::from_secs(connect_timeout_secs.unwrap_or(2));
    let idle_timeout = idle_timeout_secs
        .filter(|s| *s > 0)
        .map(Duration::from_secs);

    Ok(ParseOutcome::Run(CliConfig {
        command,
        listen: listen_addr,
        peer: peer_addr,
        params,
        profile,
        connect_timeout,
        idle_timeout,
        full_all: all,
        repeat: repeat.map(|r| r as u32).unwrap_or(1),
        repeat_set: repeat.is_some(),
    }))
}

fn parse_u64(raw: String) -> Result<u64, String> {
    raw.parse::<u64>().map_err(|_| format!("invalid number '{raw}'"))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use tokio::net::TcpStream;
    use tokio::sync::mpsc;
    use tokio::task::JoinHandle;

    fn addr(raw: &str) -> SocketAddr {
        raw.parse().unwrap()
    }

    // ---- scaffolding ------------------------------------------------------

    struct Endpoint {
        task: JoinHandle<Result<(Arc<RuntimeStats>, SocketAddr), io::Error>>,
        rx: mpsc::Receiver<TunnelEvent>,
        stats: Arc<RuntimeStats>,
        stop: Arc<AtomicBool>,
        listen: SocketAddr,
    }

    impl Endpoint {
        async fn next_event(&mut self) -> TunnelEvent {
            tokio::time::timeout(Duration::from_secs(8), self.rx.recv())
                .await
                .expect("timed out waiting for event")
                .expect("event channel closed")
        }

        async fn wait_for(&mut self, pred: fn(&TunnelEvent) -> bool) -> TunnelEvent {
            loop {
                let e = self.next_event().await;
                if pred(&e) {
                    return e;
                }
            }
        }

        async fn wait_listening(&mut self) -> SocketAddr {
            loop {
                match tokio::time::timeout(Duration::from_secs(8), self.rx.recv()).await {
                    Ok(Some(TunnelEvent::Listening { addr })) => return addr,
                    Ok(Some(_)) => continue,
                    Ok(None) => panic!("endpoint closed before Listening"),
                    Err(_) => panic!("timed out waiting for Listening"),
                }
            }
        }
    }

    async fn start_endpoint(
        role: Role,
        listen: SocketAddr,
        peer: SocketAddr,
        params: ProfileParams,
        idle: Option<Duration>,
    ) -> Endpoint {
        let (tx, rx) = mpsc::channel(256);
        let stop = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(RuntimeStats::default());
        let tx_events = tx.clone();
        let on_event: Arc<dyn Fn(TunnelEvent) + Send + Sync> = Arc::new(move |e: TunnelEvent| {
            let _ = tx_events.try_send(e);
        });
        let opts = RunOptions {
            listen,
            peer,
            params,
            connect_timeout: Duration::from_millis(2000),
            idle_timeout: idle,
        };
        let task = tokio::spawn(run_tunnel(opts, role, stop.clone(), on_event, stats.clone()));
        let mut ep = Endpoint {
            task,
            rx,
            stats,
            stop,
            listen: SocketAddr::from(([127, 0, 0, 1], 0)),
        };
        ep.listen = ep.wait_listening().await;
        ep
    }

    struct Pair {
        client_listen: SocketAddr,
        client: Endpoint,
        host: Endpoint,
    }

    async fn spawn_pair(server: SocketAddr, params: ProfileParams, idle: Option<Duration>) -> Pair {
        let host = start_endpoint(Role::Host, addr("127.0.0.1:0"), server, params, idle).await;
        let host_listen = host.listen;
        let client =
            start_endpoint(Role::Client, addr("127.0.0.1:0"), host_listen, params, idle).await;
        let client_listen = client.listen;
        Pair {
            client_listen,
            client,
            host,
        }
    }

    async fn spawn_echo() -> (SocketAddr, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            loop {
                let (stream, _) = match listener.accept().await {
                    Ok(x) => x,
                    Err(_) => return,
                };
                tokio::spawn(async move {
                    let mut s = stream;
                    let mut buf = vec![0u8; 8192];
                    loop {
                        match s.read(&mut buf).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                if s.write_all(&buf[..n]).await.is_err() {
                                    break;
                                }
                            }
                        }
                    }
                });
            }
        });
        (addr, handle)
    }

    /// Server that accepts one connection, reads a byte, then closes it.
    async fn spawn_close_server() -> (SocketAddr, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut b = [0u8; 1];
            let _ = s.read(&mut b).await;
            drop(s);
        });
        (addr, handle)
    }

    /// Address with nothing listening (for dial-failure tests).
    async fn dead_addr() -> SocketAddr {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let a = l.local_addr().unwrap();
        drop(l);
        a
    }

    /// Read up to `want` bytes (or EOF / hard 8 s deadline). Returns what we got.
    async fn read_exact_timeout(s: &mut TcpStream, want: usize) -> (usize, Vec<u8>) {
        let mut out = Vec::with_capacity(want.min(1024 * 1024));
        let mut chunk = [0u8; 8192];
        let start = tokio::time::Instant::now();
        loop {
            if out.len() >= want {
                break;
            }
            let remaining = Duration::from_secs(8).saturating_sub(start.elapsed());
            if remaining.is_zero() {
                break;
            }
            match tokio::time::timeout(remaining, s.read(&mut chunk)).await {
                Ok(Ok(0)) | Ok(Err(_)) | Err(_) => break,
                Ok(Ok(n)) => out.extend_from_slice(&chunk[..n]),
            }
        }
        (out.len(), out)
    }

    fn payload(seed: u64, size: usize) -> Vec<u8> {
        let mut r = Rng::new(seed);
        let mut v = Vec::with_capacity(size);
        while v.len() < size {
            let w = r.next_u64();
            v.extend_from_slice(&w.to_le_bytes());
        }
        v.truncate(size);
        v
    }

    fn hash(data: &[u8]) -> String {
        let mut h = Sha256::new();
        h.update(data);
        hex::encode(h.finalize())
    }

    async fn echo_once(client_listen: SocketAddr, data: &[u8]) -> Vec<u8> {
        let mut s = TcpStream::connect(client_listen).await.unwrap();
        s.write_all(data).await.unwrap();
        let (n, out) = read_exact_timeout(&mut s, data.len()).await;
        assert_eq!(n, data.len(), "echo returned a short reply");
        out
    }

    // ---- unit tests -------------------------------------------------------

    #[test]
    fn profile_values_match_documented() {
        assert_eq!(Profile::Normal.params(), ProfileParams::zero());
        let hl = Profile::HighLatency.params();
        assert_eq!((hl.latency_ms, hl.jitter_ms), (70, 15));
        let lossy = Profile::Lossy.params();
        assert_eq!(
            (lossy.latency_ms, lossy.jitter_ms, lossy.loss_percent, lossy.loss_stall_ms),
            (40, 20, 3, 250)
        );
        let vb = Profile::VeryBad.params();
        assert_eq!(
            (vb.latency_ms, vb.jitter_ms, vb.loss_percent, vb.loss_stall_ms, vb.bandwidth_kbps),
            (110, 60, 6, 500, 128)
        );
        assert_eq!(Profile::Disconnect.params().outage_after_ms, 4000);
    }

    #[test]
    fn rng_is_deterministic_and_scattered() {
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        for _ in 0..1000 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
        let mut r = Rng::new(1);
        let mut hits = 0u64;
        for _ in 0..10_000 {
            if r.chance(40) {
                hits += 1;
            }
        }
        let ratio = hits as f64 / 10_000.0;
        assert!((0.35..0.45).contains(&ratio), "chance ratio out of range: {ratio}");
    }

    #[test]
    fn conditioner_high_latency_delays_chunks() {
        let started = Instant::now();
        let mut c = Conditioner::new(Profile::HighLatency.params(), Rng::new(7), started);
        for _ in 0..20 {
            let d = c.inject_delay(Instant::now(), 1024).unwrap();
            assert!(d >= Duration::from_millis(70), "delay too small: {d:?}");
        }
    }

    #[test]
    fn conditioner_bandwidth_caps_rate() {
        let started = Instant::now();
        let params = ProfileParams {
            bandwidth_kbps: 128,
            ..ProfileParams::zero()
        };
        let mut c = Conditioner::new(params, Rng::new(3), started);
        // 128 kbps = 16000 bytes/sec. Token bucket: first chunk is free
        // (bucket full), subsequent chunks pace at chunk_size / rate.
        // Two calls: first establishes the clock, second shows pacing.
        let _ = c.inject_delay(Instant::now(), 16 * 1024);
        let d = c.inject_delay(Instant::now(), 16 * 1024).unwrap();
        assert!(d >= Duration::from_millis(950), "bandwidth pacing too small: {d:?}");
    }

    #[test]
    fn conditioner_outage_breaks_once() {
        let started = Instant::now();
        let params = ProfileParams {
            outage_after_ms: 100,
            ..ProfileParams::zero()
        };
        let mut c = Conditioner::new(params, Rng::new(9), started);
        std::thread::sleep(Duration::from_millis(150));
        assert!(c.should_break(Instant::now()));
        assert!(!c.should_break(Instant::now()));
    }

    #[test]
    fn parse_cli_defaults_by_command() {
        let host = match parse_cli(vec!["host".to_string()]).unwrap() {
            ParseOutcome::Run(c) => c,
            _ => panic!("expected Run"),
        };
        assert_eq!(host.command, Command::Host);
        assert_eq!(host.listen.to_string(), "127.0.0.1:25588");
        assert_eq!(host.peer.to_string(), "127.0.0.1:25565");

        let client = match parse_cli(vec!["client".to_string()]).unwrap() {
            ParseOutcome::Run(c) => c,
            _ => panic!("expected Run"),
        };
        assert_eq!(client.command, Command::Client);
        assert_eq!(client.listen.to_string(), "127.0.0.1:25566");
        assert_eq!(client.peer.to_string(), "127.0.0.1:25588");
    }

    #[test]
    fn parse_cli_manual_test_line() {
        let host = match parse_cli(
            "host --listen 127.0.0.1:25588 --server 127.0.0.1:25565"
                .split_whitespace()
                .map(str::to_string),
        )
        .unwrap()
        {
            ParseOutcome::Run(c) => c,
            _ => panic!("expected Run"),
        };
        assert_eq!(host.listen, addr("127.0.0.1:25588"));
        assert_eq!(host.peer, addr("127.0.0.1:25565"));

        let client = match parse_cli(
            "client --listen 127.0.0.1:25566 --host-addr 127.0.0.1:25588"
                .split_whitespace()
                .map(str::to_string),
        )
        .unwrap()
        {
            ParseOutcome::Run(c) => c,
            _ => panic!("expected Run"),
        };
        assert_eq!(client.listen, addr("127.0.0.1:25566"));
        assert_eq!(client.peer, addr("127.0.0.1:25588"));
    }

    #[test]
    fn parse_cli_rejects_bad_input() {
        assert!(parse_cli(["host".to_string(), "--listen".to_string(), "0.0.0.0:25588".to_string()]).is_err());
        assert!(parse_cli(["host".to_string(), "--listen".to_string(), "not-an-addr".to_string()]).is_err());
        assert!(parse_cli(["host".to_string(), "--loss-percent".to_string(), "150".to_string()]).is_err());
        assert!(parse_cli(vec!["--listen".to_string(), "127.0.0.1:1".to_string()]).is_err());
        assert!(parse_cli(vec!["bogus".to_string()]).is_err());
        // `--all` and `--repeat` are only valid with the `test` command.
        assert!(parse_cli(vec!["host".to_string(), "--all".to_string()]).is_err());
        assert!(parse_cli(vec!["client".to_string(), "--repeat".to_string(), "3".to_string()]).is_err());
        assert!(parse_cli(vec!["test-server".to_string(), "--all".to_string()]).is_err());
        assert!(parse_cli(vec!["test".to_string(), "--repeat".to_string(), "0".to_string()]).is_err());
        assert!(parse_cli(vec!["test".to_string(), "--all".to_string(), "--profile".to_string(), "lossy".to_string()]).is_err());
        assert!(matches!(
            parse_cli(vec!["--help".to_string()]).unwrap(),
            ParseOutcome::Help
        ));
        assert!(matches!(
            parse_cli(vec!["--version".to_string()]).unwrap(),
            ParseOutcome::Version
        ));
    }

    // ---- end-to-end tunnel tests ------------------------------------------

    #[tokio::test]
    async fn basic_echo() {
        let (server_addr, _server) = spawn_echo().await;
        let mut pair = spawn_pair(server_addr, Profile::Normal.params(), None).await;
        let reply = echo_once(pair.client_listen, b"hello world").await;
        assert_eq!(reply, b"hello world");
        let _ = (&mut pair, server_addr);
    }

    #[tokio::test]
    async fn large_message_integrity() {
        let (server_addr, _server) = spawn_echo().await;
        let pair = spawn_pair(server_addr, Profile::Normal.params(), None).await;
        let data = payload(1, 1024 * 1024);
        let reply = echo_once(pair.client_listen, &data).await;
        assert_eq!(hash(&reply), hash(&data));
    }

    #[tokio::test]
    async fn bulk_streaming() {
        let (server_addr, _server) = spawn_echo().await;
        let pair = spawn_pair(server_addr, Profile::Normal.params(), None).await;

        const TOTAL: usize = 1 * 1024 * 1024;
        let data = payload(2, TOTAL);
        let expected = hash(&data);

        // Single bidirectional connection: write all data, shutdown write, then
        // read the echoed data back.
        let mut s = TcpStream::connect(pair.client_listen).await.unwrap();
        {
            let mut s_ref = &mut s;
            for chunk in data.chunks(16 * 1024) {
                s_ref.write_all(chunk).await.unwrap();
            }
        }
        // Signal end-of-write so the echo server (and tunnel) can close.
        let _ = s.shutdown().await;

        let mut received = Vec::with_capacity(TOTAL);
        let mut buf = vec![0u8; 64 * 1024];
        while received.len() < TOTAL {
            match s.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => received.extend_from_slice(&buf[..n]),
                Err(_) => break,
            }
        }

        assert_eq!(received.len(), TOTAL, "bulk stream truncated");
        assert_eq!(hash(&received), expected);

        assert_eq!(
            pair.client.stats.bytes_upstream.load(Ordering::SeqCst),
            TOTAL as u64,
            "upstream byte accounting mismatch"
        );
        assert_eq!(
            pair.client.stats.bytes_downstream.load(Ordering::SeqCst),
            TOTAL as u64,
            "downstream byte accounting mismatch"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrency_many_sessions() {
        let (server_addr, _server) = spawn_echo().await;
        let pair = spawn_pair(server_addr, Profile::Normal.params(), None).await;

        let mut set = JoinSet::new();
        for i in 0..25u64 {
            let listen = pair.client_listen;
            set.spawn(async move {
                let data = payload(100 + i, 64 * 1024);
                let reply = echo_once(listen, &data).await;
                assert_eq!(hash(&reply), hash(&data), "concurrent session {i} corrupted");
            });
        }
        while let Some(r) = set.join_next().await {
            r.unwrap();
        }
        assert_eq!(
            pair.client.stats.sessions_total.load(Ordering::SeqCst),
            25,
            "not all sessions were accepted"
        );
    }

    #[tokio::test]
    async fn open_close_cycles() {
        let (server_addr, _server) = spawn_echo().await;
        let pair = spawn_pair(server_addr, Profile::Normal.params(), None).await;
        for i in 0..40u64 {
            let data = format!("cycle-{}", i).into_bytes();
            let reply = echo_once(pair.client_listen, &data).await;
            assert_eq!(reply, data, "cycle {i} failed");
        }
    }

    #[tokio::test]
    async fn abrupt_host_kill() {
        let (server_addr, _server) = spawn_echo().await;
        let mut pair = spawn_pair(server_addr, Profile::Normal.params(), None).await;

        let mut s = TcpStream::connect(pair.client_listen).await.unwrap();
        s.write_all(b"ping").await.unwrap();
        let (n, _) = read_exact_timeout(&mut s, 4).await;
        assert_eq!(n, 4);

        pair.host.task.abort();
        let _ = pair.host.task.await;

        // Client must notice the link is gone.
        let mut saw_break = false;
        for _ in 0..4 {
            let _ = s.write_all(b"x").await;
            let (n, _) = read_exact_timeout(&mut s, 1).await;
            if n == 0 {
                saw_break = true;
                break;
            }
        }
        assert!(saw_break, "client session did not notice host death");

        // Client process/loop stays alive and listening.
        assert!(!pair.client.task.is_finished(), "client died with the host");

        // A fresh connection now hits a dead host -> DialFailed; the local
        // socket is closed, but the client listener keeps working.
        let mut s2 = TcpStream::connect(pair.client_listen).await.unwrap();
        let _ = s2.write_all(b"again").await;
        let _ = read_exact_timeout(&mut s2, 1).await;
        let ev = pair.client.wait_for(|e| matches!(e, TunnelEvent::DialFailed { .. })).await;
        assert!(matches!(ev, TunnelEvent::DialFailed { .. }));
    }

    #[tokio::test]
    async fn abrupt_client_kill() {
        let (server_addr, _server) = spawn_echo().await;
        let mut pair = spawn_pair(server_addr, Profile::Normal.params(), None).await;

        let mut s = TcpStream::connect(pair.client_listen).await.unwrap();
        s.write_all(b"hello").await.unwrap();
        let (n, _) = read_exact_timeout(&mut s, 5).await;
        assert_eq!(n, 5);

        pair.client.task.abort();
        let _ = pair.client.task.await;

        // Host must observe the session going down and keep listening.
        let ev = pair.host.wait_for(|e| matches!(e, TunnelEvent::Disconnected { .. })).await;
        assert!(matches!(ev, TunnelEvent::Disconnected { .. }));
        assert!(!pair.host.task.is_finished(), "host died with the client");
    }

    #[tokio::test]
    async fn remote_shutdown() {
        // Minecraft server that accepts, reads one byte, then closes.
        let (server_addr, _server) = spawn_close_server().await;
        let pair = spawn_pair(server_addr, Profile::Normal.params(), None).await;

        let mut s = TcpStream::connect(pair.client_listen).await.unwrap();
        s.write_all(b"ping").await.unwrap();
        // Clean remote EOF must propagate to the client side.
        let (n, _) = read_exact_timeout(&mut s, 1).await;
        assert_eq!(n, 0, "expected EOF after server closed");
    }

    #[tokio::test]
    async fn invalid_target_dial_fails() {
        let dead = dead_addr().await;
        let mut pair = spawn_pair(dead, Profile::Normal.params(), None).await;

        let mut s = TcpStream::connect(pair.client_listen).await.unwrap();
        let _ = s.write_all(b"x").await;
        let _ = read_exact_timeout(&mut s, 1).await;

        let ev = pair.host.wait_for(|e| matches!(e, TunnelEvent::DialFailed { .. })).await;
        assert!(matches!(ev, TunnelEvent::DialFailed { .. }));
        assert_eq!(pair.host.stats.sessions_rejected.load(Ordering::SeqCst), 1);
        assert!(!pair.client.task.is_finished(), "client died after dial failure");
    }

    #[tokio::test]
    async fn idle_timeout_kills_session_and_recovers() {
        let (server_addr, _server) = spawn_echo().await;
        let idle = Duration::from_millis(200);
        let mut pair = spawn_pair(server_addr, Profile::Normal.params(), Some(idle)).await;

        // Connect and send nothing.
        let mut s = TcpStream::connect(pair.client_listen).await.unwrap();
        let _ = read_exact_timeout(&mut s, 1).await;

        let _ev = pair.client.wait_for(|e| matches!(e, TunnelEvent::Disconnected { .. })).await;
        assert!(
            pair.client.stats.idle_timeouts.load(Ordering::SeqCst) >= 1,
            "idle timeout counter not incremented"
        );

        // Next connection works normally (activity resets the timer).
        let reply = echo_once(pair.client_listen, b"alive").await;
        assert_eq!(reply, b"alive");
    }

    #[tokio::test]
    async fn reconnect_after_simulated_outage() {
        let (server_addr, _server) = spawn_echo().await;
        let params = ProfileParams {
            outage_after_ms: 300,
            ..ProfileParams::zero()
        };
        let pair = spawn_pair(server_addr, params, None).await;

        // First session works while the link is healthy.
        let mut s = TcpStream::connect(pair.client_listen).await.unwrap();
        s.write_all(b"first").await.unwrap();
        let (n, _) = read_exact_timeout(&mut s, 5).await;
        assert_eq!(n, 5);

        // Wait out the outage window; the link must drop.
        tokio::time::sleep(Duration::from_millis(700)).await;
        let (n, _) = read_exact_timeout(&mut s, 1).await;
        assert_eq!(n, 0, "expected the simulated outage to drop the session");

        // A new session reconnects cleanly.
        let mut s2 = TcpStream::connect(pair.client_listen).await.unwrap();
        s2.write_all(b"second").await.unwrap();
        let (n, r) = read_exact_timeout(&mut s2, 6).await;
        assert_eq!(n, 6);
        assert_eq!(r, b"second");
        assert!(pair.client.stats.sessions_total.load(Ordering::SeqCst) >= 2);
    }

    #[tokio::test]
    async fn lossy_and_high_latency_integrity() {
        for (profile, size) in [(Profile::Lossy, 1024 * 1024), (Profile::HighLatency, 512 * 1024)] {
            let (server_addr, _server) = spawn_echo().await;
            let pair = spawn_pair(server_addr, profile.params(), None).await;
            let data = payload(50 + size as u64, size);
            let reply = echo_once(pair.client_listen, &data).await;
            assert_eq!(
                hash(&reply),
                hash(&data),
                "integrity broken under profile {}",
                profile.name()
            );
        }
    }

    #[tokio::test]
    async fn drop_mode_corrupts_stream() {
        let (server_addr, _server) = spawn_echo().await;
        let params = ProfileParams {
            drop_percent: 30,
            ..ProfileParams::zero()
        };
        let pair = spawn_pair(server_addr, params, None).await;

        let data = payload(77, 1024 * 1024);
        let mut s = TcpStream::connect(pair.client_listen).await.unwrap();
        s.write_all(&data).await.unwrap();
        let (n, reply) = read_exact_timeout(&mut s, data.len()).await;

        // Destructive mode: some data must have been dropped on the wire.
        let up = pair.client.stats.bytes_upstream.load(Ordering::SeqCst);
        assert!(up < data.len() as u64, "no chunks dropped despite drop mode");
        assert!(
            n != data.len() || hash(&reply) != hash(&data),
            "drop mode left the stream intact unexpectedly"
        );
    }

    #[tokio::test]
    async fn e2e_mode_passes_full_pipeline() {
        // The `voidlink test` harness: real echo server, real Host/Client
        // runtime, real test-client TCP — the whole pipeline in one call.
        let logs = std::cell::RefCell::new(Vec::<String>::new());
        let summary = run_e2e_test(Profile::Normal, &|m| logs.borrow_mut().push(m.to_string()))
            .await
            .expect("E2E harness failed to start");
        let failed: Vec<(&str, Option<String>)> = summary
            .lines
            .iter()
            .filter(|l| !l.ok)
            .map(|l| (l.name.as_str(), l.reason.clone()))
            .collect();
        assert!(
            failed.is_empty(),
            "E2E checks failed: {failed:?}\nlogs:\n{}",
            logs.borrow().join("\n")
        );
        assert!(summary.passed_all());
        assert_eq!(summary.tests(), 12, "unexpected number of E2E checks");
    }

    #[test]
    fn e2e_mode_parses_test_command() {
        let cfg = match parse_cli(vec!["test".to_string(), "--profile".to_string(), "lossy".to_string()])
            .unwrap()
        {
            ParseOutcome::Run(c) => c,
            _ => panic!("expected Run"),
        };
        assert_eq!(cfg.command, Command::Test);
        assert_eq!(cfg.profile, Profile::Lossy);
    }

    #[test]
    fn e2e_mode_parses_test_server_command() {
        let cfg = match parse_cli(vec!["test-server".to_string()]).unwrap() {
            ParseOutcome::Run(c) => c,
            _ => panic!("expected Run"),
        };
        assert_eq!(cfg.command, Command::TestServer);
    }

    #[test]
    fn e2e_mode_parses_all_and_repeat() {
        let cfg = match parse_cli(vec![
            "test".to_string(),
            "--all".to_string(),
            "--repeat".to_string(),
            "10".to_string(),
        ])
        .unwrap()
        {
            ParseOutcome::Run(c) => c,
            _ => panic!("expected Run"),
        };
        assert_eq!(cfg.command, Command::Test);
        assert!(cfg.full_all);
        assert_eq!(cfg.repeat, 10);
        assert!(cfg.repeat_set);

        let plain = match parse_cli(vec!["test".to_string()]).unwrap() {
            ParseOutcome::Run(c) => c,
            _ => panic!("expected Run"),
        };
        assert!(!plain.full_all);
        assert_eq!(plain.repeat, 1);
        assert!(!plain.repeat_set);
    }

    #[tokio::test]
    async fn repeat_test_passes() {
        // Two full runs with fresh endpoints each must both pass.
        let rs = run_repeat_test(Profile::Normal, 2, &|_| {})
            .await
            .expect("repeat harness failed to start");
        assert_eq!(rs.runs, 2);
        assert_eq!(rs.passed, 2);
        assert_eq!(rs.failed, 0);
        assert!(rs.failed_details.is_empty());
    }

    #[tokio::test]
    #[ignore = "slow: runs the whole --all matrix; use -- --ignored explicitly"]
    async fn full_test_all_passes() {
        // The whole aggregated matrix (load, fuzz, abort, profiles, repeat).
        // Kept ignored so routine `cargo test` stays fast; run manually or
        // with `cargo test -- --ignored`.
        let fs = run_full_test(1, &|_| {})
            .await
            .expect("full-test harness failed to start");
        let failed: Vec<String> = fs
            .sections
            .iter()
            .flat_map(|s| {
                s.lines
                    .iter()
                    .filter(|l| !l.ok)
                    .map(|l| format!("[{}] {}: {:?}", s.title, l.name, l.reason))
            })
            .collect();
        assert!(
            failed.is_empty(),
            "full-test failures:\n{}",
            failed.join("\n")
        );
        assert!(fs.passed_all());
        assert!(!fs.sections.is_empty());
    }
}
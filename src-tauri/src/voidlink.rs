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

use std::fmt;
use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinSet;
use tokio::time::Duration;

/// Default address the Host listens on (the port the Client dials).
pub const DEFAULT_HOST_LISTEN: &str = "127.0.0.1:25588";
/// Default address of the local Minecraft server.
pub const DEFAULT_SERVER_ADDR: &str = "127.0.0.1:25565";
/// Default address the Client listens on (the port Minecraft dials).
pub const DEFAULT_CLIENT_LISTEN: &str = "127.0.0.1:25566";

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
///   - `lossy`:         40 ms / 20 ms jitter plus 3 % "loss". Loss is emulated
///                      as a short stall on the connection (bad TCP queuing /
///                      retransmission), so stream integrity is preserved.
///   - `very-bad`:      110 ms / 60 ms jitter, 6 % loss, 500 ms stalls and a
///                      hard 128 kbps bandwidth cap — a barely-playable link.
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
// CLI parsing (shared by the binary; tested here)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Host,
    Client,
}

#[derive(Debug)]
pub struct CliConfig {
    pub command: Command,
    pub listen: SocketAddr,
    pub peer: SocketAddr,
    pub params: ProfileParams,
    pub connect_timeout: Duration,
    pub idle_timeout: Option<Duration>,
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
            "listen" => listen = Some(take_value(args.next())?),
            "server" => peer = Some(take_value(args.next())?),
            "host-addr" => peer = Some(take_value(args.next())?),
            "profile" => {
                let name = take_value(args.next())?;
                profile = Profile::from_name(&name)
                    .ok_or_else(|| format!("unknown profile '{name}'"))?;
            }
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

    let command = command.ok_or_else(|| "missing command: use 'host' or 'client'".to_string())?;

    let default_listen = match command {
        Command::Host => DEFAULT_HOST_LISTEN,
        Command::Client => DEFAULT_CLIENT_LISTEN,
    };
    let listen_addr = parse_loopback_addr(listen.as_deref().unwrap_or(default_listen))?;

    let default_peer = match command {
        Command::Host => DEFAULT_SERVER_ADDR,
        Command::Client => DEFAULT_HOST_LISTEN,
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
        connect_timeout,
        idle_timeout,
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
}
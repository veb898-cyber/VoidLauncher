//! VoidLink v0.1 — local TCP tunnel CLI for VoidLauncher.
//!
//!   Minecraft Client -> VoidLink Client -> VoidLink Host -> Minecraft Server
//!
//! Current version works only between local endpoints. Internet connectivity
//! and NAT traversal are intentionally not implemented yet.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use voidlauncher_lib::voidlink::{
    self, parse_cli, CliConfig, Command, ParseOutcome, Role, RunOptions, RuntimeStats, TunnelEvent,
};

const VERSION: &str = "0.1";

fn print_event(prefix: &str, stats: &RuntimeStats, event: &TunnelEvent) {
    let bytes = stats.bytes_upstream.load(Ordering::Relaxed) + stats.bytes_downstream.load(Ordering::Relaxed);
    let tag = match event {
        TunnelEvent::Starting { role } => format!("Starting ({role})"),
        TunnelEvent::Listening { addr } => format!("Listening on {addr}"),
        TunnelEvent::Connected { id, peer } => format!("Connected #{id} from {peer}"),
        TunnelEvent::Forwarding { id, peer } => format!("Forwarding #{id} ({peer})"),
        TunnelEvent::DialFailed { id, peer, error } => {
            format!("Error #{id}: could not reach {peer}: {error}")
        }
        TunnelEvent::Disconnected { id, error } => {
            let reason = error.as_deref().unwrap_or("eof");
            format!("Disconnected #{id}: {reason}")
        }
        TunnelEvent::Stopped => format!("Stopped. total_bytes={bytes}"),
    };
    println!("[voidlink {prefix}] {tag} (bytes={bytes})");
}

fn usage() {
    println!(
        "voidlink v{VERSION} — local TCP tunnel for VoidLauncher.
USAGE:
    voidlink host   [--listen 127.0.0.1:25588] [--server 127.0.0.1:25565] [options]
    voidlink client [--listen 127.0.0.1:25566] [--host-addr 127.0.0.1:25588] [options]

COMMANDS:
    host       forwards incoming Client connections to the Minecraft server
    client     accepts Minecraft connections and dials the Host

OPTIONS:
    --listen <addr>            local address to bind (loopback only)
    --server <addr>            (host) real Minecraft server address
    --host-addr <addr>         (client) address of the VoidLink Host
    --profile <name>           normal | high-latency | lossy | very-bad | disconnect
    --latency-ms <n>           override one-way latency
    --jitter-ms <n>            override latency jitter
    --loss-percent <n>         override loss probability (0-100)
    --loss-stall-ms <n>        override loss stall length
    --drop-percent <n>         destructive mode: drop chunks (0-100)
    --bandwidth-kbps <n>       override bandwidth cap (0 = unlimited)
    --outage-after-ms <n>      override: drop link once N ms after connect
    --connect-timeout-secs <n> connect timeout (default 2)
    --idle-timeout-secs <n>    close idle sessions after N s (0 = disabled)
    --help, -h                 this help
    --version, -V              version

Examples:
    voidlink host
    voidlink client --profile lossy --bandwidth-kbps 512
    voidlink host --profile disconnect"
    );
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cfg: CliConfig = match parse_cli(args) {
        Ok(ParseOutcome::Help) => {
            usage();
            return;
        }
        Ok(ParseOutcome::Version) => {
            println!("voidlink v{VERSION}");
            return;
        }
        Ok(ParseOutcome::Run(cfg)) => cfg,
        Err(err) => {
            eprintln!("voidlink: {err}");
            eprintln!("Try 'voidlink --help' for usage.");
            std::process::exit(2);
        }
    };

    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .enable_io()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("voidlink: failed to start async runtime: {e}");
            std::process::exit(1);
        }
    };

    let code = rt.block_on(run(cfg));
    std::process::exit(code);
}

async fn run(cfg: CliConfig) -> i32 {
    let role: Role = match cfg.command {
        Command::Host => Role::Host,
        Command::Client => Role::Client,
    };

    let opts = RunOptions {
        listen: cfg.listen,
        peer: cfg.peer,
        params: cfg.params,
        connect_timeout: cfg.connect_timeout,
        idle_timeout: cfg.idle_timeout,
    };

    let stop = Arc::new(AtomicBool::new(false));
    let stop_ctrl = stop.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        eprintln!("[voidlink {role}] Ctrl+C — shutting down...");
        stop_ctrl.store(true, Ordering::SeqCst);
    });

    let stats = Arc::new(RuntimeStats::default());
    let prefix = role.to_string();
    let on_event = {
        let prefix = prefix.clone();
        let stats = stats.clone();
        Arc::new(move |event: TunnelEvent| print_event(&prefix, &stats, &event))
    };

    let start = std::time::Instant::now();
    let result = voidlink::run_tunnel(opts, role, stop, on_event, stats.clone()).await;

    match result {
        Ok((stats, listen)) => {
            let elapsed = start.elapsed().as_secs_f64();
            let up = stats.bytes_upstream.load(Ordering::Relaxed) as f64;
            let down = stats.bytes_downstream.load(Ordering::Relaxed) as f64;
            let total = up + down;
            let throughput = total / (1024.0 * 1024.0) / elapsed.max(1e-9);
            println!(
                "[voidlink {prefix}] Summary: listen={listen} elapsed={elapsed:.1}s \
                 up={up:.0}B down={down:.0}B total={}MiB throughput={throughput:.2}MiB/s \
                 sessions={} rejected={} errored={} idle_timeouts={} peak_concurrent={}",
                total / (1024.0 * 1024.0),
                stats.sessions_total.load(Ordering::Relaxed),
                stats.sessions_rejected.load(Ordering::Relaxed),
                stats.sessions_errored.load(Ordering::Relaxed),
                stats.idle_timeouts.load(Ordering::Relaxed),
                stats.peak_concurrent.load(Ordering::Relaxed),
            );
            0
        }
        Err(e) => {
            eprintln!("[voidlink {prefix}] Error: {e}");
            1
        }
    }
}
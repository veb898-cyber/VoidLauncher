//! VoidLink v0.1 — local TCP tunnel CLI for VoidLauncher.
//!
//!   Minecraft Client -> VoidLink Client -> VoidLink Host -> Minecraft Server
//!
//! Current version works only between local endpoints. Internet connectivity
//! and NAT traversal are intentionally not implemented yet.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tokio::net::TcpListener;
use voidlauncher_lib::voidlink::{
    self, parse_cli, CliConfig, Command, ParseOutcome, Profile, Role, RunOptions, RuntimeStats,
    TunnelEvent,
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
    voidlink host        [--listen 127.0.0.1:25588] [--server 127.0.0.1:25565] [options]
    voidlink client      [--listen 127.0.0.1:25566] [--host-addr 127.0.0.1:25588] [options]
    voidlink test        [--profile <name>] [--all] [--repeat <n>]   automatic E2E test
    voidlink test-server [--listen 127.0.0.1:25589]   standalone echo TCP endpoint

COMMANDS:
    host         forwards incoming Client connections to the Minecraft server
    client       accepts Minecraft connections and dials the Host
    test         E2E self-test (echo server + Host + Client + test client);
                 with --all runs the full matrix (load, fuzz, abort, profiles,
                 repeat sweep)
    test-server  binds a plain TCP echo endpoint (NOT a Minecraft server)

OPTIONS:
    --listen <addr>            local address to bind (loopback only)
    --server <addr>            (host) real Minecraft server address
    --host-addr <addr>         (client) address of the VoidLink Host
    --profile <name>           normal | high-latency | lossy | very-bad | disconnect
    --all                      (with `test`) full test matrix; cannot be
                               combined with --profile
    --repeat <n>               (with `test`) repeat the test n times, fresh
                               endpoints each run (default for --all: 3)
    --latency-ms <n>           override one-way latency
    --jitter-ms <n>            override latency jitter
    --loss-percent <n>         override \"loss\" (STALL, bytes preserved) probability (0-100)
    --loss-stall-ms <n>        override how long a \"loss\" event stalls the link
    --drop-percent <n>         DESTRUCTIVE test: silently drop chunks, corrupting the stream (0-100)
    --bandwidth-kbps <n>       override bandwidth cap (0 = unlimited)
    --outage-after-ms <n>      override: drop link once N ms after connect
    --connect-timeout-secs <n> connect timeout (default 2)
    --idle-timeout-secs <n>    close idle sessions after N s (0 = disabled)
    --help, -h                 this help
    --version, -V              version

Terminology:
    \"loss\" is NOT packet loss — it stalls the link like TCP retransmission;
    all bytes are still delivered, stream integrity is preserved.
    \"drop\" is a destructive stress mode: bytes vanish, integrity is broken.

Examples:
    voidlink host
    voidlink client --profile lossy --bandwidth-kbps 512
    voidlink host --profile disconnect
    voidlink test
    voidlink test --profile very-bad
    voidlink test --all
    voidlink test --repeat 10"
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
    match cfg.command {
        Command::Test => {
            if cfg.full_all {
                run_full_test_mode(cfg.repeat, cfg.repeat_set).await
            } else {
                run_e2e_mode(cfg.profile, cfg.repeat).await
            }
        }
        Command::TestServer => run_test_server_mode(cfg.listen).await,
        Command::Host | Command::Client => run_tunnel_mode(cfg).await,
    }
}

async fn run_tunnel_mode(cfg: CliConfig) -> i32 {
    let role: Role = match cfg.command {
        Command::Host => Role::Host,
        Command::Client => Role::Client,
        _ => unreachable!("run_tunnel_mode only handles host/client"),
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

/// One-shot end-to-end test: embedded echo server + Host + Client + test
/// client, all over real TCP. Prints a PASS/FAIL report and returns 0 only if
/// every check passed. With `repeat > 1` runs the whole pipeline that many
/// times, each run with completely fresh endpoints.
async fn run_e2e_mode(profile: Profile, repeat: u32) -> i32 {
    if repeat > 1 {
        return run_e2e_repeat_mode(profile, repeat).await;
    }

    println!("VoidLink E2E test");
    println!("-----------------");
    let summary = match voidlink::run_e2e_test(profile, &|msg| println!("{msg}")).await {
        Ok(s) => s,
        Err(e) => {
            println!("[FATAL] the E2E harness could not start: {e}");
            println!("\nEND-TO-END TEST FAILED");
            return 1;
        }
    };

    for line in &summary.lines {
        if line.ok {
            println!("[PASS] {}", line.name);
        } else {
            println!("[FAIL] {}", line.name);
            if let Some(reason) = &line.reason {
                println!("Reason: {reason}");
            }
        }
    }

    println!();
    println!("Tests: {}", summary.tests());
    println!("Passed: {}", summary.passed());
    println!("Failed: {}", summary.failed());
    if summary.passed_all() {
        println!("\nEND-TO-END TEST PASSED");
        0
    } else {
        println!("\nEND-TO-END TEST FAILED");
        1
    }
}

/// `voidlink test --repeat N`: N full pipeline runs, each with its own
/// endpoints, aggregated into a Runs/Passed/Failed summary. Failed runs print
/// their full check list and diagnostics.
async fn run_e2e_repeat_mode(profile: Profile, repeat: u32) -> i32 {
    println!("VoidLink E2E test (repeat {repeat})");
    println!("------------------------------------");
    let rs = match voidlink::run_repeat_test(profile, repeat, &|msg| println!("  {msg}")).await {
        Ok(rs) => rs,
        Err(e) => {
            println!("[FATAL] the E2E harness could not start: {e}");
            println!("\nEND-TO-END TEST FAILED");
            return 1;
        }
    };

    for (idx, lines) in &rs.failed_details {
        println!("\nrun {idx} failed:");
        for l in lines {
            if l.ok {
                println!("  [PASS] {}", l.name);
            } else {
                println!("  [FAIL] {}", l.name);
                if let Some(reason) = &l.reason {
                    println!("    Reason: {reason}");
                }
            }
        }
    }

    println!();
    println!("Runs: {} / Passed: {} / Failed: {}", rs.runs, rs.passed, rs.failed);
    if rs.failed == 0 {
        println!("\nEND-TO-END TEST PASSED");
        0
    } else {
        println!("\nEND-TO-END TEST FAILED");
        1
    }
}

/// `voidlink test --all`: the full test matrix, aggregated into titled sections
/// (basic E2E, heavier load, fuzz, abort scenarios, every profile, repeat sweep).
async fn run_full_test_mode(repeat: u32, repeat_set: bool) -> i32 {
    let repeat = if repeat_set { repeat } else { 3 };
    println!("VoidLink full test (--all)");
    println!("===========================");
    let fs = match voidlink::run_full_test(repeat, &|msg| println!("  {msg}")).await {
        Ok(fs) => fs,
        Err(e) => {
            println!("\n[FATAL] the full-test harness could not start: {e}");
            println!("\nRESULT: FAIL");
            return 1;
        }
    };

    for section in &fs.sections {
        println!("\n[section] {}", section.title);
        for line in &section.lines {
            if line.ok {
                println!("  [PASS] {}", line.name);
            } else {
                println!("  [FAIL] {}", line.name);
                if let Some(reason) = &line.reason {
                    println!("    Reason: {reason}");
                }
            }
        }
        if let Some(note) = &section.note {
            println!("  {note}");
        }
    }

    println!();
    println!(
        "Summary: {} checks across {} sections",
        fs.tests(),
        fs.sections.len()
    );
    println!("Passed: {}", fs.passed());
    println!("Failed: {}", fs.failed());
    if fs.passed_all() {
        println!("\nRESULT: PASS");
        0
    } else {
        println!("\nRESULT: FAIL");
        1
    }
}

/// Standalone echo TCP endpoint (a generic test server, NOT a Minecraft
/// server). Echoes every byte back until Ctrl+C.
async fn run_test_server_mode(listen: std::net::SocketAddr) -> i32 {
    let listener = match TcpListener::bind(listen).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("[voidlink test-server] bind {listen}: {e}");
            return 1;
        }
    };
    let addr = listener.local_addr().unwrap();
    println!("[voidlink test-server] listening on {addr} (Ctrl+C to stop)");

    let stop = Arc::new(AtomicBool::new(false));
    let stop_ctrl = stop.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        eprintln!("[voidlink test-server] Ctrl+C — shutting down...");
        stop_ctrl.store(true, Ordering::SeqCst);
    });

    match voidlink::run_echo_server(listener, stop).await {
        Ok(addr) => {
            println!("[voidlink test-server] Stopped ({addr}).");
            0
        }
        Err(e) => {
            eprintln!("[voidlink test-server] Error: {e}");
            1
        }
    }
}
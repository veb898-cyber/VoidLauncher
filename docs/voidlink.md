# VoidLink v0.1

Local TCP tunnel for VoidLauncher — routes Minecraft traffic between two
processes on the same machine without the game ever knowing.

## Architecture

```
Minecraft Client ──TCP──► VoidLink Client ──TCP──► VoidLink Host ──TCP──► Minecraft Server
  (connects to               (listens locally,           (listens for client,
   127.0.0.1:25566)           dials the host)             dials the server)
```

Every hop is a raw, unframed TCP byte stream. Minecraft does not know a tunnel
exists.

## How it works

Each side of the tunnel (`run_tunnel`) accepts connections and spawns a paired
session (`handle_session`). Inside a session:

1. **Local → tunnel relay** — reads from the local Minecraft side and writes
   into the tunnel socket. On the Client this direction carries upstream
   (MC → server) traffic; on the Host it carries downstream (server → MC)
   traffic.

2. **Tunnel → local relay** — reads from the tunnel socket and writes to the
   local Minecraft side. The opposite direction.

Both relays run as independent tasks. When one relay finishes (EOF or error),
the session waits up to **5 seconds** for the other relay to complete, then
aborts. This allows in-flight data (e.g. echo responses) to drain before the
session is torn down.

### Simulated network

The tunnel includes an optional **conditioner** that applies artificial latency,
jitter, bandwidth caps, and loss to one direction of the tunnel socket. Loss is
emulated as a TCP-like stall (bytes are still delivered, just delayed), so
**stream integrity is preserved under all default profiles**. A separate
destructive mode (`--drop-percent`) can intentionally corrupt the stream for
stress testing.

## CLI

```
voidlink host   [--listen 127.0.0.1:25588] [--server 127.0.0.1:25565] [options]
voidlink client [--listen 127.0.0.1:25566] [--host-addr 127.0.0.1:25588] [options]
```

All addresses are **loopback-only** (127.0.0.1 or ::1). External addresses are
rejected at parse time.

### Options

| Flag | Default | Description |
|------|---------|-------------|
| `--listen` | host:25588 / client:25566 | Local address to bind |
| `--server` | 127.0.0.1:25565 | Minecraft server address (host only) |
| `--host-addr` | 127.0.0.1:25588 | Host address (client only) |
| `--profile` | normal | Network profile |
| `--latency-ms` | from profile | Override one-way latency |
| `--jitter-ms` | from profile | Override jitter |
| `--loss-percent` | from profile | Override loss probability (0-100) |
| `--loss-stall-ms` | from profile | Override stall length |
| `--drop-percent` | 0 | Destructive chunk drop probability |
| `--bandwidth-kbps` | 0 (unlimited) | Bandwidth cap |
| `--outage-after-ms` | 0 (never) | Break link once after N ms |
| `--connect-timeout-secs` | 2 | Dial timeout |
| `--idle-timeout-secs` | 0 (disabled) | Close idle sessions |

### Status output

Events are printed as `[voidlink host]` / `[voidlink client]` with one of:

- **Starting** — role and listen address
- **Listening** — bound address
- **Connected** — accepted a connection from a peer
- **Forwarding** — session established, data flowing
- **DialFailed** — could not reach the remote side
- **Disconnected** — session ended (with reason)
- **Stopped** — run loop exited (Ctrl+C or stop flag)

## Automated tests

Run all VoidLink tests:

```bash
cargo test voidlink -- --test-threads=2
```

Test count: **21 unit/integration tests** covering:

| Test | What it verifies |
|------|-----------------|
| `profile_values_match_documented` | All 5 named profiles have correct parameter values |
| `rng_is_deterministic_and_scattered` | Deterministic seed output; `chance()` distribution |
| `conditioner_high_latency_delays_chunks` | High-latency profile adds ≥70 ms per chunk |
| `conditioner_bandwidth_caps_rate` | 128 kbps cap paces 16 KiB chunks at ~1 s |
| `conditioner_outage_breaks_once` | Outage fires exactly once after threshold |
| `parse_cli_defaults_by_command` | Default addresses correct for host/client |
| `parse_cli_manual_test_line` | Manual test commands parse correctly |
| `parse_cli_rejects_bad_input` | Non-loopback, bad ports, missing command, --help, --version |
| `basic_echo` | Single 11-byte echo through the tunnel |
| `large_message_integrity` | 1 MiB payload echoed with SHA-256 integrity check |
| `bulk_streaming` | 1 MiB bidirectional streaming with SHA-256 integrity |
| `concurrency_many_sessions` | 25 concurrent echo sessions, all with correct integrity |
| `open_close_cycles` | 40 sequential short-lived sessions |
| `abrupt_host_kill` | Host killed mid-session; client detects and survives |
| `abrupt_client_kill` | Client killed mid-session; host detects and survives |
| `remote_shutdown` | Server closes; EOF propagates through tunnel to client |
| `invalid_target_dial_fails` | Host dials dead port; DialFailed event; client stays up |
| `idle_timeout_kills_session_and_recovers` | Idle 200ms timeout fires; next connection works |
| `reconnect_after_simulated_outage` | Disconnect profile breaks session; reconnect works |
| `lossy_and_high_latency_integrity` | Integrity under lossy (1 MiB) and high-latency (512 KiB) |
| `drop_mode_corrupts_stream` | Destructive 30% drop breaks stream integrity |

## Stress test profiles

| Profile | Latency | Jitter | Loss | Stall | Bandwidth | Outage |
|---------|---------|--------|------|-------|-----------|--------|
| `normal` | 0 ms | 0 ms | 0% | 0 ms | unlimited | never |
| `high-latency` | 70 ms | 15 ms | 0% | 0 ms | unlimited | never |
| `lossy` | 40 ms | 20 ms | 3% | 250 ms | unlimited | never |
| `very-bad` | 110 ms | 60 ms | 6% | 500 ms | 128 kbps | never |
| `disconnect` | 0 ms | 0 ms | 0% | 0 ms | unlimited | 4000 ms |

**Parameter justification:**

- **high-latency (70/15)**: Plausible high-ping link (e.g. cross-continent Wi-Fi).
- **lossy (40/20/3%/250ms)**: Moderate congestion. Loss emulated as 250 ms stall
  (TCP retransmission + congestion backoff), so integrity is preserved.
- **very-bad (110/60/6%/500ms/128kbps)**: Worst tolerable mobile link.
  128 kbps bandwidth cap is a hard ceiling; all other params stacked.
- **disconnect (outage 4000ms)**: Simulates a connection dropping exactly once
  4 s after establishment. Client is expected to reconnect.

Override any parameter via CLI flags (e.g. `--loss-percent 10 --loss-stall-ms 500`).

## Manual Minecraft test

**Requirements:**
- Minecraft Java Edition server running on LAN (default port 25565)
- `voidlink.exe` built and accessible

**Step 1 — Start the Host:**
```powershell
.\voidlink.exe host --listen 127.0.0.1:25588 --server 127.0.0.1:25565
```
Expected output:
```
[voidlink host] Starting (host)
[voidlink host] Listening on 127.0.0.1:25588
```

**Step 2 — Start the Client (new terminal):**
```powershell
.\voidlink.exe client --listen 127.0.0.1:25566 --host-addr 127.0.0.1:25588
```
Expected output:
```
[voidlink client] Starting (client)
[voidlink client] Listening on 127.0.0.1:25566
```

**Step 3 — Connect Minecraft:**
1. Open Minecraft → Multiplayer → Add Server
2. Server address: `127.0.0.1:25566`
3. Join the server

**Expected result:**
- Both terminals show `Connected` → `Forwarding` events
- Minecraft loads and joins the world normally
- Byte counters (`bytes=`) in the event lines increase as you move/break blocks

**Step 4 — Verify traffic goes through VoidLink:**
- Check the `bytes=` values in both terminals — they should be non-zero and
  growing
- Disconnect from the server — both terminals show `Disconnected`
- Check active connections: `netstat -ano | findstr 25588` should show no
  lingering connections

**Step 5 — Stop:**
- Press `Ctrl+C` in both terminal windows
- Both show `Stopped` with a summary line (total bytes, throughput, sessions)

## Limitations

- **Local only**: Both endpoints must run on the same machine. Internet
  connectivity and NAT traversal are intentionally not implemented yet.
- **No encryption**: The tunnel is a raw TCP stream. The architecture must
  allow adding TLS or another encryption layer in a future version.
- **Loopback only**: All addresses must be 127.0.0.1 or ::1. External
  interfaces are rejected at parse time.
- **No Minecraft protocol awareness**: The tunnel is protocol-agnostic.
  It does not parse packets, compress data, or validate frames.

## Known issues

- Debug builds are significantly slower than release builds for bulk transfers.
  Use `cargo build --release --bin voidlink` for manual testing.
- The `concurrency_many_sessions` test asserts `sessions_total == 25` (all
  sessions accepted) rather than a peak concurrency metric, because session
  durations on localhost are too short to guarantee high peak concurrency.

## Next stage requirements

- **NAT traversal / STUN**: For tunnelling between machines on different
  networks.
- **Encryption**: Optional TLS layer on the tunnel socket.
- **Session multiplexing**: Multiple Minecraft connections over one TCP socket.
- **Web UI**: Status dashboard showing live session stats and throughput.

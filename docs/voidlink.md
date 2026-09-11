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
voidlink host        [--listen 127.0.0.1:25588] [--server 127.0.0.1:25565] [options]
voidlink client      [--listen 127.0.0.1:25566] [--host-addr 127.0.0.1:25588] [options]
voidlink test        [--profile <name>] [--all] [--repeat <n>]   automatic end-to-end test
voidlink test-server [--listen 127.0.0.1:25589]   standalone echo TCP endpoint
```

`test` runs the whole pipeline automatically (embedded echo server + VoidLink
Host + VoidLink Client + test client) and stops itself — no Minecraft server,
no second PC, no external programs, no internet needed. `test --all` runs the
full test matrix (load, fuzz, abort scenarios, every profile, a repeat sweep);
`test --repeat <n>` runs the standard pipeline n times, each run with
completely fresh endpoints. `test-server` is a plain TCP echo endpoint for
manual experiments.

All addresses are **loopback-only** (127.0.0.1 or ::1). External addresses are
rejected at parse time.

### Options

| Flag | Default | Description |
|------|---------|-------------|
| `--listen` | host:25588 / client:25566 / test-server:25589 | Local address to bind |
| `--server` | 127.0.0.1:25565 | Minecraft server address (host only) |
| `--host-addr` | 127.0.0.1:25588 | Host address (client only) |
| `--profile` | normal | Network profile (test: ignore when `--all` is used) |
| `--all` | off | With `test`: run the full matrix (load, fuzz, abort, every profile, repeat sweep). Cannot be combined with `--profile`. Endpoints are `:0` (auto-assigned ports). |
| `--repeat <n>` | 1 (3 for `--all`) | With `test`: run the pipeline n times, each run in a fresh harness with its own endpoints. Aggregated `Runs / Passed / Failed` footer; failed runs print full diag. n ≥ 1. |
| `--latency-ms` | from profile | Override one-way latency |
| `--jitter-ms` | from profile | Override jitter |
| `--loss-percent` | from profile | Override "loss" probability (0-100). Loss = **stall**, bytes preserved. |
| `--loss-stall-ms` | from profile | Override stall length of a "loss" event |
| `--drop-percent` | 0 | **Destructive** test: silently drop chunks, corrupting the stream (0-100) |
| `--bandwidth-kbps` | 0 (unlimited) | Bandwidth cap |
| `--outage-after-ms` | 0 (never) | Break link once after N ms |
| `--connect-timeout-secs` | 2 | Dial timeout |
| `--idle-timeout-secs` | 0 (disabled) | Close idle sessions |

## Terminology (exact, not marketing)

- **Latency / jitter** — an artificial one-way delay added to each chunk.
- **Loss** — is **NOT packet loss**. Every "loss" event only *stalls* the
  connection for `loss-stall-ms` (emulating a TCP retransmission / bad
  queueing). All bytes are still delivered; **stream integrity is preserved**
  under every default profile, including `very-bad`.
- **Drop (`--drop-percent`)** — a **destructive stress test**: a chunk is
  silently discarded, so the byte stream is corrupted on purpose. This is
  **not** a realistic simulation of network packet loss.

## Status output

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

Test count: **27 VoidLink tests** (26 run by default + 1 ignored full-matrix
test), covering:

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
| `e2e_mode_parses_test_command` | `test --profile x` parses into `Command::Test` |
| `e2e_mode_parses_test_server_command` | `test-server` parses into `Command::TestServer` |
| `e2e_mode_parses_all_and_repeat` | `test --all` / `test --repeat n` parse and validate correctly |
| `e2e_mode_passes_full_pipeline` | The whole `voidlink test` harness passes under `normal` |
| `repeat_test_passes` | `run_repeat_test(normal, 2)` — two full runs, fresh endpoints, both pass |
| `full_test_all_passes` (ignored) | The whole `--all` matrix passes (run with `-- --ignored`) |
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

## Automated E2E testing

### Why `voidlink test` exists

The unit and integration tests above already exercise the tunnel engine, but
they run inside the same process as the test harness. `voidlink test` is a
**real TCP end-to-end run**: it starts four genuinely separate network
components over real sockets —

```
Test Client → VoidLink Client → VoidLink Host → Test Server (echo)
```

and verifies real bytes travel through the whole chain in both directions,
including the return path:

```
Test Server → VoidLink Host → VoidLink Client → Test Client
```

### What it checks

One command runs the entire pipeline:

```bash
voidlink test
```

Checks under `normal`:

1. small payload (11 B);
2. minimal payload (1 B);
3. 1 KiB;
4. 64 KiB;
5. 1 MiB;
6. bidirectional duplex (simultaneous write + read on one connection);
7. 10 sequential connections;
8. 25 concurrent connections with per-session integrity;
9. graceful close (clean EOF propagation);
10. reconnect after close;
11. error handling (dialing a closed port must fail cleanly);
12. cleanup (no listener may remain bound after teardown).

On failure, the report shows which check failed, how many bytes were received
vs expected, the TCP error, and a trace of the last Host/Client events.

### Profiles

```bash
voidlink test --profile normal
voidlink test --profile high-latency
voidlink test --profile lossy
voidlink test --profile very-bad
voidlink test --profile disconnect
```

The stable profiles run the same 12 checks; on slow links the payload sizes
are scaled down (e.g. `very-bad` uses 16 KiB / 64 KiB instead of 64 KiB /
1 MiB because of its 128 kbps cap). The `disconnect` profile would break long
transfers on purpose (the link drops once 4 s after connect), so it runs a
recovery suite instead: healthy session → outage drops it → reconnect works →
error handling.

### Full test matrix: `voidlink test --all`

```bash
voidlink test --all
```

Runs the whole battery in one process and prints one aggregated report
(`Summary: N checks across M sections`, then `RESULT: PASS/FAIL`). Every
section uses a completely fresh harness (its own echo server / Host / Client
on auto-assigned ports). Sections and what they cover:

| Section | What it verifies |
|---------|------------------|
| Basic E2E | the standard 12 checks under `normal` |
| Heavier load | 1 MiB and 8 MiB payloads, 20 connect/close cycles, 50 and 100 concurrent sessions, 1 MiB duplex, recovery echo |
| Fuzz payloads | random payload sizes 1 B..256 KiB and 1 B..1 MiB, byte-exact + SHA-256 |
| Abort: abnormal client close | abrupt client close (ungraceful, no clean EOF) mid-transfer is torn down; tunnel recovers |
| Abort: host shutdown | Host stops mid-transfer; the reset/EOF is surfaced instead of hanging |
| Abort: echo server shutdown | echo dies mid-transfer; restart proves no stale state, dial-refused handled |
| Error classification | clean remote close → EOF; silent peer → timeout (not a hang); refused dial → connect error |
| Profile high-latency / lossy / very-bad | the stable suites under each profile |
| Profile disconnect | recovery suite under the outage profile |
| Repeat | 3 full runs (`--repeat`), fresh endpoints each |

The abort suites stop a component mid-transfer and verify the connection is
torn down with an observable EOF/reset/timeout on the client — never a hang —
exactly the error surface a Minecraft client would see.

### Repeating runs: `voidlink test --repeat <n>`

```bash
voidlink test --repeat 10
voidlink test --all --repeat 5   # full matrix, 5-run sweep
```

Each run is a genuinely fresh pipeline: new echo server, new Host, new
Client, new test client, auto-assigned ports, full teardown between runs. The
footer is `Runs: 10 / Passed: 10 / Failed: 0`; any failed run prints its check
list with reasons. Failed runs keep per-run diagnostics — a single flaky run
does not hide the failing check.

### A Minecraft server is NOT needed

The embedded **Test Server** is a plain TCP *echo* endpoint. It does not
pretend to be a Minecraft server and it is not one. That is a feature: the
test proves VoidLink proxies an arbitrary TCP byte stream end to end, nothing
more.

### What success does and does NOT prove

A green `voidlink test` proves VoidLink's **TCP proxying** is correct: bytes
flow, integrity holds, close and reconnect work, scaling to 25 sessions. It
does **NOT** prove Minecraft compatibility — Minecraft speaks its own
protocol (handshake, status/ping, compression, encryption), and this test
never speaks it. The only way to prove Minecraft compatibility is the manual
Minecraft test below.

### Manual test server

```bash
voidlink test-server --listen 127.0.0.1:25589
```

Binds a plain echo endpoint (NOT a Minecraft server) for manual experiments,
e.g. `Test-NetConnection 127.0.0.1 -Port 25589` or a raw-socket client.

## Stress test profiles

| Profile | Latency | Jitter | Loss (stall) | Stall | Bandwidth | Outage |
|---------|---------|--------|--------------|-------|-----------|--------|
| `normal` | 0 ms | 0 ms | 0% | 0 ms | unlimited | never |
| `high-latency` | 70 ms | 15 ms | 0% | 0 ms | unlimited | never |
| `lossy` | 40 ms | 20 ms | 3% | 250 ms | unlimited | never |
| `very-bad` | 110 ms | 60 ms | 6% | 500 ms | 128 kbps | never |
| `disconnect` | 0 ms | 0 ms | 0% | 0 ms | unlimited | 4000 ms |

There is no byte-dropping in any profile above — the "Loss (stall)" column is
exactly what it says: a percentage of chunks that get delayed by `Stall`
milliseconds. Stream integrity is always preserved. Byte dropping exists only
as the **destructive** `--drop-percent` override.

**Parameter justification:**

- **high-latency (70/15)**: Plausible high-ping link (e.g. cross-continent Wi-Fi).
- **lossy (40/20/3%/250ms)**: Moderate congestion. Each "loss" is emulated as
  a 250 ms stall (TCP retransmission + congestion backoff); bytes preserved.
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

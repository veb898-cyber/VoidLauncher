/**
 * Server status polling for the saved-server list.
 *
 * The launcher never joins a server to check it: every check is a Server
 * List Ping (`cmd_ping_server`) with a hard budget on the Rust side, so a
 * dead server can only yield "timed out" / "couldn't check", never a hang.
 * This module owns the client-side scheduling around those calls:
 *
 *   - refresh on demand (list loaded / edited, «Обновить») and on a slow
 *     60 s interval while the tab is open;
 *   - at most `concurrency` requests in flight, extra addresses wait;
 *   - one in-flight request per address (double refresh does not duplicate);
 *   - a per-address sequence number drops late replies after the address
 *     was edited, removed or the poller was destroyed;
 *   - while a re-check runs, the previous values are kept and marked
 *     `checking` so the UI does not flash empty numbers.
 */

import type { MessageKey } from './i18n';

export type PingState = 'online' | 'timeout' | 'unreachable' | 'error';

/** Raw result of `cmd_ping_server` (Rust `server_ping::PingResult`). */
export interface PingResult {
  state: PingState;
  ping_ms: number | null;
  players_online: number | null;
  players_max: number | null;
  version_name: string | null;
  detail: string | null;
}

export interface ServerStatus {
  /** True while a re-check is in flight. The other fields keep the last
   *  known values until the new result arrives (or replace them on error). */
  checking: boolean;
  state?: PingState;
  pingMs?: number | null;
  playersOnline?: number | null;
  playersMax?: number | null;
  versionName?: string | null;
  updatedAt?: number;
}

interface ServerStatusPollerOptions {
  ping: (address: string) => Promise<PingResult>;
  /** `null` means "forget this address" (removed from the list). */
  onUpdate: (address: string, status: ServerStatus | null) => void;
  concurrency?: number;
  /** 0 disables the periodic re-check (tests drive `refresh()` instead). */
  intervalMs?: number;
}

export class ServerStatusPoller {
  private addresses: string[] = [];
  private queue: string[] = [];
  private inFlight = new Map<string, number>();
  private seq = new Map<string, number>();
  private last = new Map<string, ServerStatus>();
  private generation = 0;
  private timer: ReturnType<typeof setInterval> | null = null;

  constructor(private readonly options: ServerStatusPollerOptions) {
    if (options.intervalMs && options.intervalMs > 0) {
      this.timer = setInterval(() => this.refresh(), options.intervalMs);
    }
  }

  /** Sets the tracked list; new addresses are checked, removed ones are
   *  forgotten. Existing results are kept (no flicker on list reload). */
  setAddresses(addresses: string[]): void {
    const next = [...new Set(addresses.map((a) => a.trim()).filter(Boolean))];
    const nextSet = new Set(next);

    for (const previous of this.addresses) {
      if (!nextSet.has(previous)) {
        this.seq.set(previous, (this.seq.get(previous) ?? 0) + 1);
        this.last.delete(previous);
        // A pending reply for this address is stale from now on; freeing the
        // slot lets the address be checked fresh if it is added back.
        this.inFlight.delete(previous);
        this.options.onUpdate(previous, null);
      }
    }

    this.addresses = next;
    this.queue = this.queue.filter((a) => nextSet.has(a));
    for (const address of next) {
      if (!this.last.has(address) && !this.inFlight.has(address) && !this.queue.includes(address)) {
        this.queue.push(address);
      }
    }
    this.pump();
  }

  /** Forces a re-check of every tracked address (manual «Обновить»). */
  refresh(): void {
    for (const address of this.addresses) {
      if (!this.inFlight.has(address) && !this.queue.includes(address)) {
        this.queue.push(address);
      }
    }
    this.pump();
  }

  /** Stops the interval and invalidates every in-flight reply. */
  destroy(): void {
    this.generation += 1;
    if (this.timer) {
      clearInterval(this.timer);
      this.timer = null;
    }
    this.queue = [];
  }

  private pump(): void {
    const limit = Math.max(1, this.options.concurrency ?? 4);
    while (this.inFlight.size < limit && this.queue.length > 0) {
      this.start(this.queue.shift()!);
    }
  }

  private start(address: string): void {
    const generation = this.generation;
    const seq = (this.seq.get(address) ?? 0) + 1;
    this.seq.set(address, seq);
    this.inFlight.set(address, seq);

    const previous = this.last.get(address);
    this.emit(address, { ...(previous ?? {}), checking: true });

    this.options
      .ping(address)
      .then(
        (result) => {
          if (!this.isCurrent(address, generation, seq)) return;
          this.emit(address, {
            checking: false,
            state: result.state,
            pingMs: result.ping_ms,
            playersOnline: result.players_online,
            playersMax: result.players_max,
            versionName: result.version_name,
            updatedAt: Date.now(),
          });
        },
        () => {
          // The backend rejected the request itself (not a server verdict):
          // report a check failure without inventing numbers.
          if (!this.isCurrent(address, generation, seq)) return;
          this.emit(address, { checking: false, state: 'error', updatedAt: Date.now() });
        },
      )
      .finally(() => {
        if (this.inFlight.get(address) === seq) {
          this.inFlight.delete(address);
          this.pump();
        }
      });
  }

  private isCurrent(address: string, generation: number, seq: number): boolean {
    return (
      generation === this.generation &&
      this.seq.get(address) === seq &&
      this.addresses.includes(address)
    );
  }

  private emit(address: string, status: ServerStatus): void {
    this.last.set(address, status);
    this.options.onUpdate(address, status);
  }
}

/** Human label for one status cell. */
export function statusLabel(
  status: ServerStatus | undefined,
  t: (key: MessageKey, vars?: Record<string, string>) => string,
): string {
  if (!status || (status.checking && status.state === undefined)) {
    return t('servers.status_checking');
  }
  switch (status.state) {
    case 'online': {
      const online = status.playersOnline;
      const max = status.playersMax;
      if (typeof online === 'number' && typeof max === 'number') {
        return t('servers.status_online', { online: String(online), max: String(max) });
      }
      return t('servers.status_online_plain');
    }
    case 'timeout':
      return t('servers.status_timeout');
    case 'unreachable':
      return t('servers.status_unreachable');
    default:
      return t('servers.status_error');
  }
}

/** Secondary latency line, only for a successful check. */
export function statusPing(
  status: ServerStatus | undefined,
  t: (key: MessageKey, vars?: Record<string, string>) => string,
): string | null {
  if (status?.state !== 'online' || typeof status.pingMs !== 'number') return null;
  return t('servers.ping_ms', { ms: String(status.pingMs) });
}

/** Signal-bar level (1..5) for a measured latency, with the classic
 *  Minecraft server-list thresholds. */
export function pingBarsLevel(pingMs: number): number {
  if (pingMs < 150) return 5;
  if (pingMs < 300) return 4;
  if (pingMs < 600) return 3;
  if (pingMs < 1000) return 2;
  return 1;
}

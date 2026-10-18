import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  ServerStatusPoller,
  statusLabel,
  statusPing,
  pingBarsLevel,
  type PingResult,
  type ServerStatus,
} from './serverStatus';
import { t } from './i18n';
import { useLanguageStore } from '../stores/languageStore';

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

const onlineResult = (online = 1, max = 20, ping: number | null = 42): PingResult => ({
  state: 'online',
  ping_ms: ping,
  players_online: online,
  players_max: max,
  version_name: '1.21.4',
  detail: null,
});

interface Recorder {
  updates: Array<[string, ServerStatus | null]>;
  last: (address: string) => ServerStatus | null | undefined;
}

function makeRecorder(): Recorder {
  const updates: Array<[string, ServerStatus | null]> = [];
  return {
    updates,
    last: (address) => {
      const hit = [...updates].reverse().find(([a]) => a === address);
      return hit ? hit[1] : undefined;
    },
  };
}

afterEach(() => {
  useLanguageStore.setState({ language: 'en' });
  vi.useRealTimers();
});

describe('ServerStatusPoller', () => {
  it('checks every address once and publishes the parsed result', async () => {
    const recorder = makeRecorder();
    const ping = vi.fn(async (address: string) => onlineResult(address === 'a' ? 3 : 5, 100));
    const poller = new ServerStatusPoller({
      ping,
      onUpdate: (address, status) => recorder.updates.push([address, status]),
    });

    poller.setAddresses(['a', 'b']);
    await flush();

    expect(ping).toHaveBeenCalledTimes(2);
    expect(recorder.last('a')).toMatchObject({
      checking: false,
      state: 'online',
      playersOnline: 3,
      playersMax: 100,
      pingMs: 42,
    });
    expect(recorder.last('b')?.playersOnline).toBe(5);
    poller.destroy();
  });

  it('marks the address as checking before the first result arrives', async () => {
    const recorder = makeRecorder();
    const poller = new ServerStatusPoller({
      ping: async () => onlineResult(),
      onUpdate: (address, status) => recorder.updates.push([address, status]),
    });

    poller.setAddresses(['a']);
    expect(recorder.last('a')).toEqual({ checking: true });
    await flush();
    poller.destroy();
  });

  it('keeps the previous values while a re-check runs', async () => {
    const recorder = makeRecorder();
    const resolvers: Array<(result: PingResult) => void> = [];
    const ping = vi.fn(() => new Promise<PingResult>((resolve) => resolvers.push(resolve)));
    const poller = new ServerStatusPoller({
      ping,
      onUpdate: (address, status) => recorder.updates.push([address, status]),
    });

    poller.setAddresses(['a']);
    resolvers[0](onlineResult(4, 50));
    await flush();

    poller.refresh();
    expect(recorder.last('a')).toMatchObject({
      checking: true,
      state: 'online',
      playersOnline: 4,
    });

    resolvers[1](onlineResult(9, 50));
    await flush();
    expect(recorder.last('a')).toMatchObject({ checking: false, playersOnline: 9 });
    poller.destroy();
  });

  it('does not duplicate an in-flight check on repeated refresh', async () => {
    const ping = vi.fn(() => new Promise<PingResult>(() => {}));
    const poller = new ServerStatusPoller({ ping, onUpdate: () => {} });

    poller.setAddresses(['a']);
    poller.refresh();
    poller.refresh();

    expect(ping).toHaveBeenCalledTimes(1);
    poller.destroy();
  });

  it('limits the number of parallel checks', async () => {
    const resolvers: Array<(result: PingResult) => void> = [];
    const ping = vi.fn(() => new Promise<PingResult>((resolve) => resolvers.push(resolve)));
    const poller = new ServerStatusPoller({ ping, onUpdate: () => {}, concurrency: 2 });

    poller.setAddresses(['a', 'b', 'c', 'd']);
    expect(ping).toHaveBeenCalledTimes(2);

    resolvers[0](onlineResult());
    await flush();
    expect(ping).toHaveBeenCalledTimes(3);

    resolvers[1](onlineResult());
    await flush();
    expect(ping).toHaveBeenCalledTimes(4);
    poller.destroy();
  });

  it('forgets removed addresses and ignores their late replies', async () => {
    const recorder = makeRecorder();
    const resolvers: Array<(result: PingResult) => void> = [];
    const ping = vi.fn(() => new Promise<PingResult>((resolve) => resolvers.push(resolve)));
    const poller = new ServerStatusPoller({
      ping,
      onUpdate: (address, status) => recorder.updates.push([address, status]),
    });

    poller.setAddresses(['a']);
    poller.setAddresses([]);
    expect(recorder.last('a')).toBeNull();

    resolvers[0](onlineResult(8, 8));
    await flush();
    expect(recorder.last('a')).toBeNull();
    poller.destroy();
  });

  it('drops a late reply when the address was re-added before it arrived', async () => {
    const recorder = makeRecorder();
    const calls: Array<{ resolve: (result: PingResult) => void }> = [];
    const ping = vi.fn(
      () => new Promise<PingResult>((resolve) => calls.push({ resolve })),
    );
    const poller = new ServerStatusPoller({
      ping,
      onUpdate: (address, status) => recorder.updates.push([address, status]),
    });

    poller.setAddresses(['a']);
    poller.setAddresses([]);
    poller.setAddresses(['a']);
    expect(ping).toHaveBeenCalledTimes(2);

    calls[0].resolve(onlineResult(1, 1)); // stale request
    await flush();
    expect(recorder.last('a')).toEqual({ checking: true });

    calls[1].resolve(onlineResult(7, 7));
    await flush();
    expect(recorder.last('a')).toMatchObject({ playersOnline: 7 });
    poller.destroy();
  });

  it('reports a check failure without inventing numbers', async () => {
    const recorder = makeRecorder();
    const poller = new ServerStatusPoller({
      ping: async () => {
        throw new Error('address is not in this instance\'s server list');
      },
      onUpdate: (address, status) => recorder.updates.push([address, status]),
    });

    poller.setAddresses(['a']);
    await flush();
    expect(recorder.last('a')).toEqual({
      checking: false,
      state: 'error',
      updatedAt: expect.any(Number),
    });
    poller.destroy();
  });

  it('ignores every reply after destroy', async () => {
    const recorder = makeRecorder();
    const resolvers: Array<(result: PingResult) => void> = [];
    const ping = vi.fn(() => new Promise<PingResult>((resolve) => resolvers.push(resolve)));
    const poller = new ServerStatusPoller({
      ping,
      onUpdate: (address, status) => recorder.updates.push([address, status]),
    });

    poller.setAddresses(['a']);
    poller.destroy();
    resolvers[0](onlineResult());
    await flush();
    expect(recorder.updates).toHaveLength(1);
    expect(recorder.last('a')).toEqual({ checking: true });
  });

  it('re-checks all addresses on the interval until destroyed', async () => {
    vi.useFakeTimers();
    const ping = vi.fn(async () => onlineResult());
    const poller = new ServerStatusPoller({ ping, onUpdate: () => {}, intervalMs: 30_000 });

    poller.setAddresses(['a']);
    await vi.advanceTimersByTimeAsync(0);
    expect(ping).toHaveBeenCalledTimes(1);

    await vi.advanceTimersByTimeAsync(30_000);
    expect(ping).toHaveBeenCalledTimes(2);

    poller.destroy();
    await vi.advanceTimersByTimeAsync(120_000);
    expect(ping).toHaveBeenCalledTimes(2);
  });
});

describe('statusLabel', () => {
  it('maps every state to an honest English label', () => {
    expect(statusLabel(undefined, t)).toBe('Checking…');
    expect(statusLabel({ checking: true }, t)).toBe('Checking…');
    expect(statusLabel({ checking: false, state: 'online', playersOnline: 3, playersMax: 20 }, t)).toBe('Online: 3/20');
    expect(statusLabel({ checking: false, state: 'online' }, t)).toBe('Online');
    expect(statusLabel({ checking: false, state: 'timeout' }, t)).toBe('Timed out');
    expect(statusLabel({ checking: false, state: 'unreachable' }, t)).toBe('Couldn’t connect');
    expect(statusLabel({ checking: false, state: 'error' }, t)).toBe('Check failed');
  });

  it('keeps showing the previous result while re-checking', () => {
    expect(
      statusLabel({ checking: true, state: 'online', playersOnline: 4, playersMax: 50 }, t),
    ).toBe('Online: 4/50');
  });

  it('speaks Russian when the launcher does', () => {
    useLanguageStore.setState({ language: 'ru' });
    expect(statusLabel({ checking: true }, t)).toBe('Проверяется…');
    expect(statusLabel({ checking: false, state: 'online', playersOnline: 1, playersMax: 2 }, t)).toBe('Онлайн: 1/2');
    expect(statusLabel({ checking: false, state: 'timeout' }, t)).toBe('Тайм-аут');
    expect(statusLabel({ checking: false, state: 'unreachable' }, t)).toBe('Не удалось подключиться');
    expect(statusLabel({ checking: false, state: 'error' }, t)).toBe('Ошибка проверки');
  });
});

describe('statusPing', () => {
  it('returns the latency only for a successful check', () => {
    expect(statusPing({ checking: false, state: 'online', pingMs: 42 }, t)).toBe('Ping: 42 ms');
    expect(statusPing({ checking: false, state: 'online' }, t)).toBeNull();
    expect(statusPing({ checking: false, state: 'timeout' }, t)).toBeNull();
    expect(statusPing(undefined, t)).toBeNull();
  });
});

describe('pingBarsLevel', () => {
  it('scales the bar count by latency like the vanilla server list', () => {
    expect(pingBarsLevel(0)).toBe(5);
    expect(pingBarsLevel(149)).toBe(5);
    expect(pingBarsLevel(150)).toBe(4);
    expect(pingBarsLevel(299)).toBe(4);
    expect(pingBarsLevel(300)).toBe(3);
    expect(pingBarsLevel(599)).toBe(3);
    expect(pingBarsLevel(600)).toBe(2);
    expect(pingBarsLevel(999)).toBe(2);
    expect(pingBarsLevel(1000)).toBe(1);
    expect(pingBarsLevel(5000)).toBe(1);
  });
});

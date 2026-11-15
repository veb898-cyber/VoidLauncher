import { beforeEach, describe, expect, it } from 'vitest';
import {
  PLAYER_HEAD_STORAGE_KEY,
  PLAYER_SKIN_STORAGE_KEY,
  PLAYER_HEAD_TTL_MS,
  __playerHeadTest,
  getPlayerHeadState,
  getPlayerSkinState,
  forgetPlayerHead,
  invalidatePlayerHead,
  requestPlayerHead,
  refreshPlayerHead,
  subscribePlayerHead,
  subscribePlayerSkin,
} from './playerHeadStore';
import { useSkinSettingsStore } from '../stores/skinSettingsStore';

const UUID_A = '1a650540-fb59-4d9d-a1d3-f8f7a67379a3';
const UUID_B = '2b650540-fb59-4d9d-a1d3-f8f7a67379a4';

interface Deferred<T> {
  promise: Promise<T>;
  resolve: (value: T) => void;
  reject: (reason?: unknown) => void;
}

function deferred<T>(): Deferred<T> {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

const tick = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

function last<T>(values: T[]): T | undefined {
  return values.length > 0 ? values[values.length - 1] : undefined;
}

let now = 0;
let calls: string[] = [];
let getImpl: () => Promise<string | null> = async () => 'SKIN';
let invalidateImpl: () => Promise<unknown> = async () => 1;

function installRuntime(): void {
  __playerHeadTest.setRuntime({
    now: () => now,
    crop: async (_uuid, skin) => skin,
    invoke: async (cmd) => {
      calls.push(cmd);
      if (cmd === 'cmd_get_player_head') return getImpl();
      if (cmd === 'cmd_invalidate_player_head') return invalidateImpl();
      throw new Error(`unexpected command: ${cmd}`);
    },
  });
}

beforeEach(() => {
  __playerHeadTest.reset();
  localStorage.clear();
  useSkinSettingsStore.setState({ refreshMode: 'auto' });
  now = 0;
  calls = [];
  getImpl = async () => 'SKIN';
  invalidateImpl = async () => 1;
  installRuntime();
});

describe('player head invalidation', () => {
  it('discards an old in-flight response and notifies every subscriber with the new head', async () => {
    const first = deferred<string | null>();
    const backend = deferred<unknown>();
    const second = deferred<string | null>();
    let getCount = 0;
    getImpl = () => {
      getCount += 1;
      return getCount === 1 ? first.promise : second.promise;
    };
    invalidateImpl = () => backend.promise;

    const heads: Array<string | null> = [];
    const skins: Array<string | null> = [];
    subscribePlayerHead(UUID_A, (head) => heads.push(head));
    subscribePlayerSkin(UUID_A, (skin) => skins.push(skin));

    const initial = requestPlayerHead(UUID_A);
    await tick();
    expect(getCount).toBe(1);

    const invalidation = invalidatePlayerHead(UUID_A);
    await tick();
    expect(calls).toContain('cmd_invalidate_player_head');

    // The old request completes while the backend invalidation is pending.
    first.resolve('OLD_SKIN');
    await tick();
    expect(__playerHeadTest.head(UUID_A)).toBeNull();
    expect(__playerHeadTest.skin(UUID_A)).toBeNull();

    backend.resolve(1);
    await tick();
    expect(getCount).toBe(2);

    second.resolve('NEW_SKIN');
    await expect(invalidation).resolves.toBe(true);
    await expect(initial).resolves.toBeNull();

    expect(__playerHeadTest.head(UUID_A)).toBe('NEW_SKIN');
    expect(__playerHeadTest.skin(UUID_A)).toBe('NEW_SKIN');
    expect(heads).not.toContain('OLD_SKIN');
    expect(skins).not.toContain('OLD_SKIN');
    expect(last(heads)).toBe('NEW_SKIN');
    expect(last(skins)).toBe('NEW_SKIN');
  });

  it('gates a request made during backend invalidation until the old disk entry is gone', async () => {
    const backend = deferred<unknown>();
    const fresh = deferred<string | null>();
    let getCount = 0;
    getImpl = () => {
      getCount += 1;
      return fresh.promise;
    };
    invalidateImpl = () => backend.promise;

    const invalidation = invalidatePlayerHead(UUID_A);
    await tick();
    const request = requestPlayerHead(UUID_A);
    await tick();

    expect(getCount).toBe(0);
    expect(__playerHeadTest.isInvalidating(UUID_A)).toBe(true);

    backend.resolve(1);
    await tick();
    expect(getCount).toBe(1);

    fresh.resolve('FRESH_SKIN');
    await expect(request).resolves.toBe('FRESH_SKIN');
    await expect(invalidation).resolves.toBe(true);
    expect(__playerHeadTest.head(UUID_A)).toBe('FRESH_SKIN');
  });

  it('does not let an older request block a forced request, and never caches the older result', async () => {
    const old = deferred<string | null>();
    const fresh = deferred<string | null>();
    let getCount = 0;
    getImpl = () => {
      getCount += 1;
      return getCount === 1 ? old.promise : fresh.promise;
    };

    const initial = requestPlayerHead(UUID_A);
    await tick();
    const invalidation = invalidatePlayerHead(UUID_A);
    await tick();
    expect(getCount).toBe(2);

    fresh.resolve('NEW_SKIN');
    await expect(invalidation).resolves.toBe(true);
    expect(__playerHeadTest.head(UUID_A)).toBe('NEW_SKIN');

    old.resolve('OLD_SKIN');
    await expect(initial).resolves.toBeNull();
    expect(__playerHeadTest.head(UUID_A)).toBe('NEW_SKIN');
    expect(__playerHeadTest.skin(UUID_A)).toBe('NEW_SKIN');
  });

  it('keeps all avatar subscribers on the same fresh image', async () => {
    const first: Array<string | null> = [];
    const second: Array<string | null> = [];
    subscribePlayerHead(UUID_A, (head) => first.push(head));
    subscribePlayerHead(UUID_A, (head) => second.push(head));

    await expect(invalidatePlayerHead(UUID_A)).resolves.toBe(true);

    expect(last(first)).toBe('SKIN');
    expect(last(second)).toBe('SKIN');
  });

  it('updates a subscriber that attached while the image was already cached', async () => {
    // This is the exact 2D regression: a cached Avatar used to skip the
    // listener registration and therefore never saw another component's
    // refresh.
    await expect(requestPlayerHead(UUID_A)).resolves.toBe('SKIN');
    const heads: Array<string | null> = [];
    subscribePlayerHead(UUID_A, (head) => heads.push(head));

    getImpl = async () => 'NEW_SKIN';
    await expect(invalidatePlayerHead(UUID_A)).resolves.toBe(true);

    expect(last(heads)).toBe('NEW_SKIN');
    expect(__playerHeadTest.head(UUID_A)).toBe('NEW_SKIN');
  });

  it('updates the 3D raw skin and the 2D head from one backend request', async () => {
    const heads: Array<string | null> = [];
    const skins: Array<string | null> = [];
    subscribePlayerHead(UUID_A, (head) => heads.push(head));
    subscribePlayerSkin(UUID_A, (skin) => skins.push(skin));

    const first = requestPlayerHead(UUID_A);
    const second = requestPlayerHead(UUID_A);
    await expect(first).resolves.toBe('SKIN');
    await expect(second).resolves.toBe('SKIN');

    expect(calls.filter((c) => c === 'cmd_get_player_head')).toHaveLength(1);
    expect(last(heads)).toBe('SKIN');
    expect(last(skins)).toBe('SKIN');
    expect(__playerHeadTest.head(UUID_A)).toBe('SKIN');
    expect(__playerHeadTest.skin(UUID_A)).toBe('SKIN');
  });

  it('fetches the raw skin when only the 2D head cache is warm', async () => {
    __playerHeadTest.seedHead(UUID_A, 'OLD_HEAD');
    await expect(requestPlayerHead(UUID_A)).resolves.toBe('SKIN');
    expect(calls.filter((c) => c === 'cmd_get_player_head')).toHaveLength(1);
    expect(__playerHeadTest.skin(UUID_A)).toBe('SKIN');
  });

  it('handles a second invalidation before the first refresh finishes', async () => {
    const firstBackend = deferred<unknown>();
    const secondBackend = deferred<unknown>();
    let invalidateCount = 0;
    invalidateImpl = () => {
      invalidateCount += 1;
      return invalidateCount === 1 ? firstBackend.promise : secondBackend.promise;
    };
    let getCount = 0;
    getImpl = async () => {
      getCount += 1;
      return 'FINAL_SKIN';
    };

    const first = invalidatePlayerHead(UUID_A);
    await tick();
    const second = invalidatePlayerHead(UUID_A);
    await tick();
    expect(invalidateCount).toBe(2);

    firstBackend.resolve(1);
    await tick();
    // The superseded refresh must not fetch or clear state.
    expect(getCount).toBe(0);

    secondBackend.resolve(2);
    await tick();
    expect(getCount).toBe(1);
    await Promise.all([first, second]);

    expect(__playerHeadTest.head(UUID_A)).toBe('FINAL_SKIN');
    expect(__playerHeadTest.skin(UUID_A)).toBe('FINAL_SKIN');
  });

  it('does not touch another account when one UUID is invalidated', async () => {
    __playerHeadTest.seedHead(UUID_B, 'B_HEAD');
    __playerHeadTest.seedSkin(UUID_B, 'B_SKIN');
    __playerHeadTest.seedHead(UUID_A, 'A_HEAD');
    __playerHeadTest.seedSkin(UUID_A, 'A_SKIN');

    await expect(invalidatePlayerHead(UUID_A)).resolves.toBe(true);

    expect(__playerHeadTest.head(UUID_B)).toBe('B_HEAD');
    expect(__playerHeadTest.skin(UUID_B)).toBe('B_SKIN');
    expect(__playerHeadTest.generation(UUID_B)).toBe(0);
    expect(__playerHeadTest.head(UUID_A)).toBe('SKIN');
  });

  it('keeps the old image when backend invalidation fails and reports the error', async () => {
    __playerHeadTest.seedHead(UUID_A, 'OLD_HEAD');
    __playerHeadTest.seedSkin(UUID_A, 'OLD_SKIN');
    invalidateImpl = async () => {
      throw new Error('backend unavailable');
    };

    await expect(invalidatePlayerHead(UUID_A)).rejects.toThrow('backend unavailable');

    expect(__playerHeadTest.head(UUID_A)).toBe('OLD_HEAD');
    expect(__playerHeadTest.skin(UUID_A)).toBe('OLD_SKIN');
    expect(getPlayerHeadState(UUID_A)).toEqual({ src: 'OLD_HEAD', status: 'ready' });

    // A later request may serve the still-working renderer entry without
    // pretending that a backend invalidation succeeded.
    await expect(requestPlayerHead(UUID_A)).resolves.toBe('OLD_HEAD');
    expect(calls).not.toContain('cmd_get_player_head');
  });
});

describe('player head TTL', () => {
  it('keeps the old image visible while an expired entry is revalidated', async () => {
    __playerHeadTest.seedHead(UUID_A, 'OLD_HEAD', 0);
    __playerHeadTest.seedSkin(UUID_A, 'OLD_SKIN', 0);
    now = PLAYER_HEAD_TTL_MS + 1;

    const fresh = deferred<string | null>();
    getImpl = () => fresh.promise;
    const request = requestPlayerHead(UUID_A);
    await tick();

    expect(calls.filter((c) => c === 'cmd_get_player_head')).toHaveLength(1);
    expect(getPlayerHeadState(UUID_A)).toEqual({
      src: 'OLD_HEAD',
      status: 'ready',
      refreshing: true,
    });
    expect(getPlayerSkinState(UUID_A)).toEqual({
      skin: 'OLD_SKIN',
      status: 'ready',
      refreshing: true,
    });

    fresh.resolve('NEW_SKIN');
    await expect(request).resolves.toBe('NEW_SKIN');
    expect(getPlayerHeadState(UUID_A)).toEqual({ src: 'NEW_SKIN', status: 'ready' });
    expect(getPlayerSkinState(UUID_A)).toEqual({ skin: 'NEW_SKIN', status: 'ready' });
  });

  it('preserves a working image when an automatic revalidation fails', async () => {
    __playerHeadTest.seedHead(UUID_A, 'OLD_HEAD', 0);
    __playerHeadTest.seedSkin(UUID_A, 'OLD_SKIN', 0);
    now = PLAYER_HEAD_TTL_MS + 1;

    const heads: Array<string | null> = [];
    subscribePlayerHead(UUID_A, (head) => heads.push(head));
    getImpl = async () => {
      throw new Error('network down');
    };

    await expect(requestPlayerHead(UUID_A)).resolves.toBeNull();

    expect(__playerHeadTest.head(UUID_A)).toBe('OLD_HEAD');
    expect(__playerHeadTest.skin(UUID_A)).toBe('OLD_SKIN');
    expect(heads).not.toContain(null);
    expect(getPlayerHeadState(UUID_A)).toEqual({ src: 'OLD_HEAD', status: 'ready' });
    expect(__playerHeadTest.isRefreshing(UUID_A)).toBe(false);
  });

  it('cancels the refresh overlay when the revalidation fails', async () => {
    __playerHeadTest.seedHead(UUID_A, 'OLD_HEAD', 0);
    now = PLAYER_HEAD_TTL_MS + 1;
    getImpl = async () => {
      throw new Error('network down');
    };

    await requestPlayerHead(UUID_A);

    expect(getPlayerHeadState(UUID_A)).toEqual({ src: 'OLD_HEAD', status: 'ready' });
    expect(__playerHeadTest.isRefreshing(UUID_A)).toBe(false);
  });

  it('explicit invalidation bypasses a fresh renderer cache', async () => {
    __playerHeadTest.seedHead(UUID_A, 'OLD_HEAD');
    __playerHeadTest.seedSkin(UUID_A, 'OLD_SKIN');

    await expect(invalidatePlayerHead(UUID_A)).resolves.toBe(true);

    expect(calls).toContain('cmd_invalidate_player_head');
    expect(calls).toContain('cmd_get_player_head');
    expect(__playerHeadTest.head(UUID_A)).toBe('SKIN');
  });

  it('notifies the 3D skin channel when the first fetch fails', async () => {
    const skins: Array<string | null> = [];
    subscribePlayerSkin(UUID_A, (skin) => skins.push(skin));
    getImpl = async () => {
      throw new Error('network down');
    };

    await expect(requestPlayerHead(UUID_A)).resolves.toBeNull();

    expect(skins).toContain(null);
    expect(getPlayerSkinState(UUID_A).status).toBe('failed');
    expect(getPlayerHeadState(UUID_A).status).toBe('failed');
  });

  it('marks both channels failed when invalidation fails with empty caches', async () => {
    invalidateImpl = async () => {
      throw new Error('backend unavailable');
    };
    const heads: Array<string | null> = [];
    const skins: Array<string | null> = [];
    subscribePlayerHead(UUID_A, (head) => heads.push(head));
    subscribePlayerSkin(UUID_A, (skin) => skins.push(skin));

    await expect(invalidatePlayerHead(UUID_A)).rejects.toThrow('backend unavailable');

    expect(heads).toContain(null);
    expect(skins).toContain(null);
    expect(getPlayerHeadState(UUID_A).status).toBe('failed');
    expect(getPlayerSkinState(UUID_A).status).toBe('failed');
    expect(calls).not.toContain('cmd_get_player_head');
  });

  it('forgets a removed account without starting a network refresh', () => {
    __playerHeadTest.seedHead(UUID_A, 'OLD_HEAD');
    __playerHeadTest.seedSkin(UUID_A, 'OLD_SKIN');
    const heads: Array<string | null> = [];
    const skins: Array<string | null> = [];
    subscribePlayerHead(UUID_A, (head) => heads.push(head));
    subscribePlayerSkin(UUID_A, (skin) => skins.push(skin));

    forgetPlayerHead(UUID_A);

    expect(__playerHeadTest.head(UUID_A)).toBeNull();
    expect(__playerHeadTest.skin(UUID_A)).toBeNull();
    expect(heads).toContain(null);
    expect(skins).toContain(null);
    expect(calls).not.toContain('cmd_get_player_head');
  });

  it('has a TTL no longer than a day', () => {
    expect(PLAYER_HEAD_TTL_MS).toBeGreaterThan(0);
    expect(PLAYER_HEAD_TTL_MS).toBeLessThanOrEqual(24 * 60 * 60 * 1000);
  });
});

describe('player head persistence', () => {
  const HEAD_A = 'data:image/png;base64,AAAA';
  const HEAD_B = 'data:image/png;base64,BBBB';
  const keyOf = (uuid: string) => uuid.replace(/-/g, '').toLowerCase();

  it('saves a successful head and restores it without waiting for the network', async () => {
    getImpl = async () => HEAD_A;
    await expect(requestPlayerHead(UUID_A)).resolves.toBe(HEAD_A);

    expect(__playerHeadTest.persistedHead(UUID_A)).toBe(HEAD_A);

    calls = [];
    __playerHeadTest.reloadPersisted();
    expect(getPlayerHeadState(UUID_A)).toEqual({ src: HEAD_A, status: 'ready' });
    expect(calls).not.toContain('cmd_get_player_head');
  });

  it('replaces the persisted head when a newer skin arrives', async () => {
    __playerHeadTest.seedHead(UUID_A, HEAD_A);
    getImpl = async () => HEAD_B;

    await expect(requestPlayerHead(UUID_A)).resolves.toBe(HEAD_B);

    expect(__playerHeadTest.head(UUID_A)).toBe(HEAD_B);
    expect(__playerHeadTest.persistedHead(UUID_A)).toBe(HEAD_B);
  });

  it('keeps the last good head when the background fetch fails', async () => {
    __playerHeadTest.seedHead(UUID_A, HEAD_A);
    getImpl = async () => {
      throw new Error('network down');
    };

    await expect(requestPlayerHead(UUID_A)).resolves.toBeNull();

    expect(getPlayerHeadState(UUID_A)).toEqual({ src: HEAD_A, status: 'ready' });
    expect(__playerHeadTest.isRefreshing(UUID_A)).toBe(false);
  });

  it('ignores a corrupt persisted entry', () => {
    localStorage.setItem(
      PLAYER_HEAD_STORAGE_KEY,
      JSON.stringify({ [keyOf(UUID_A)]: { value: 'not-an-image', at: 1 } }),
    );
    __playerHeadTest.reloadPersisted();

    expect(getPlayerHeadState(UUID_A)).toEqual({ src: null, status: 'loading' });
  });

  it('keys the persisted heads by uuid, not by player name', async () => {
    getImpl = async () => HEAD_A;
    await requestPlayerHead(UUID_A);
    getImpl = async () => HEAD_B;
    await requestPlayerHead(UUID_B);

    expect(__playerHeadTest.persistedHead(UUID_A)).toBe(HEAD_A);
    expect(__playerHeadTest.persistedHead(UUID_B)).toBe(HEAD_B);
  });

  it('shares one network request between concurrent callers', async () => {
    const pending = deferred<string | null>();
    let count = 0;
    getImpl = () => {
      count += 1;
      return pending.promise;
    };

    const first = requestPlayerHead(UUID_A);
    const second = requestPlayerHead(UUID_A);
    await tick();
    expect(count).toBe(1);

    pending.resolve(HEAD_A);
    await expect(first).resolves.toBe(HEAD_A);
    await expect(second).resolves.toBe(HEAD_A);
  });

  it('keeps the normal placeholder for an account without any cache', () => {
    expect(getPlayerHeadState(UUID_A)).toEqual({ src: null, status: 'loading' });
  });
});

describe('persisted full skin textures', () => {
  const SKIN_A = 'data:image/png;base64,AAAA';
  const SKIN_B = 'data:image/png;base64,BBBB';

  it('saves the full texture and restores it without a network request', async () => {
    getImpl = async () => SKIN_A;
    await requestPlayerHead(UUID_A, { provider: 'Microsoft' });

    expect(__playerHeadTest.persistedSkin(UUID_A, 'Microsoft')).toBe(SKIN_A);

    calls = [];
    __playerHeadTest.reloadPersisted();
    expect(getPlayerSkinState(UUID_A, 'Microsoft')).toEqual({
      skin: SKIN_A,
      status: 'ready',
    });

    await requestPlayerHead(UUID_A, { provider: 'Microsoft' });
    expect(calls).not.toContain('cmd_get_player_head');
  });

  it('updates the 3D texture together with the head from one response', async () => {
    getImpl = async () => SKIN_A;
    await requestPlayerHead(UUID_A, { provider: 'Microsoft' });

    expect(__playerHeadTest.head(UUID_A)).toBe(SKIN_A);
    expect(__playerHeadTest.skin(UUID_A, 'Microsoft')).toBe(SKIN_A);
  });

  it('separates caches for different providers even with the same uuid', async () => {
    getImpl = async () => SKIN_A;
    await requestPlayerHead(UUID_A, { provider: 'Microsoft' });
    getImpl = async () => SKIN_B;
    await requestPlayerHead(UUID_A, { provider: 'Offline' });

    expect(__playerHeadTest.persistedSkin(UUID_A, 'Microsoft')).toBe(SKIN_A);
    expect(__playerHeadTest.persistedSkin(UUID_A, 'Offline')).toBe(SKIN_B);

    __playerHeadTest.reloadPersisted();
    expect(getPlayerSkinState(UUID_A, 'Microsoft').skin).toBe(SKIN_A);
    expect(getPlayerSkinState(UUID_A, 'Offline').skin).toBe(SKIN_B);
  });

  it('shows the cached skin and skips automatic refresh in manual mode', async () => {
    __playerHeadTest.seedHead(UUID_A, SKIN_A);
    __playerHeadTest.seedSkin(UUID_A, SKIN_A, 0, 'Microsoft');
    useSkinSettingsStore.setState({ refreshMode: 'manual' });
    calls = [];

    await requestPlayerHead(UUID_A, { provider: 'Microsoft' });

    expect(calls).not.toContain('cmd_get_player_head');
    expect(getPlayerSkinState(UUID_A, 'Microsoft')).toEqual({
      skin: SKIN_A,
      status: 'ready',
    });
  });

  it('auto-refreshes only after the 24 hour cache window', async () => {
    __playerHeadTest.seedHead(UUID_A, SKIN_A);
    __playerHeadTest.seedSkin(UUID_A, SKIN_A);
    now = PLAYER_HEAD_TTL_MS - 1;
    getImpl = async () => SKIN_B;

    await requestPlayerHead(UUID_A);
    expect(calls).not.toContain('cmd_get_player_head');

    now = PLAYER_HEAD_TTL_MS + 1;
    await requestPlayerHead(UUID_A);
    expect(calls).toContain('cmd_get_player_head');
    expect(__playerHeadTest.skin(UUID_A)).toBe(SKIN_B);
  });

  it('keeps the old texture when a background update fails', async () => {
    __playerHeadTest.seedHead(UUID_A, SKIN_A);
    __playerHeadTest.seedSkin(UUID_A, SKIN_A);
    now = PLAYER_HEAD_TTL_MS + 1;
    getImpl = async () => {
      throw new Error('network down');
    };

    await requestPlayerHead(UUID_A);

    expect(getPlayerSkinState(UUID_A)).toEqual({ skin: SKIN_A, status: 'ready' });
    expect(__playerHeadTest.isRefreshing(UUID_A)).toBe(false);
  });

  it('ignores a corrupt persisted full texture', () => {
    const key = UUID_A.replace(/-/g, '').toLowerCase();
    localStorage.setItem(
      PLAYER_SKIN_STORAGE_KEY,
      JSON.stringify({ [`microsoft:${key}`]: { value: 'not-an-image', at: 1 } }),
    );
    __playerHeadTest.reloadPersisted();

    expect(getPlayerSkinState(UUID_A, 'Microsoft')).toEqual({
      skin: null,
      status: 'loading',
    });
  });

  it('refreshes a Microsoft or Ely.by skin only on an explicit request in manual mode', async () => {
    useSkinSettingsStore.setState({ refreshMode: 'manual' });
    __playerHeadTest.seedHead(UUID_A, SKIN_A);
    __playerHeadTest.seedSkin(UUID_A, SKIN_A, 0, 'ElyBy');
    getImpl = async () => SKIN_B;

    await requestPlayerHead(UUID_A, { provider: 'ElyBy' });
    expect(__playerHeadTest.skin(UUID_A, 'ElyBy')).toBe(SKIN_A);

    await invalidatePlayerHead(UUID_A, 'ElyBy');
    expect(__playerHeadTest.skin(UUID_A, 'ElyBy')).toBe(SKIN_B);
  });

  it('does not start a network request for cached offline skins in manual mode', async () => {
    useSkinSettingsStore.setState({ refreshMode: 'manual' });
    __playerHeadTest.seedHead(UUID_B, SKIN_B);
    __playerHeadTest.seedSkin(UUID_B, SKIN_B, 0, 'Offline');
    calls = [];

    await requestPlayerHead(UUID_B, { provider: 'Offline' });

    expect(calls).not.toContain('cmd_get_player_head');
  });

  it('rebuilds a missing head from the cached texture without the network', async () => {
    useSkinSettingsStore.setState({ refreshMode: 'manual' });
    __playerHeadTest.seedSkin(UUID_A, SKIN_A, 0, 'Microsoft');
    calls = [];

    await expect(requestPlayerHead(UUID_A, { provider: 'Microsoft' })).resolves.toBe(SKIN_A);

    expect(calls).not.toContain('cmd_get_player_head');
    expect(__playerHeadTest.head(UUID_A)).toBe(SKIN_A);
  });

  it('manual refresh replaces both the head and the 3D texture on success', async () => {
    __playerHeadTest.seedHead(UUID_A, SKIN_A);
    __playerHeadTest.seedSkin(UUID_A, SKIN_A, 0, 'Microsoft');
    getImpl = async () => SKIN_B;

    await expect(refreshPlayerHead(UUID_A, 'Microsoft')).resolves.toBe(true);

    expect(__playerHeadTest.head(UUID_A)).toBe(SKIN_B);
    expect(__playerHeadTest.skin(UUID_A, 'Microsoft')).toBe(SKIN_B);
  });

  it('manual refresh keeps the old image and reports failure when the network fails', async () => {
    __playerHeadTest.seedHead(UUID_A, SKIN_A);
    __playerHeadTest.seedSkin(UUID_A, SKIN_A, 0, 'Microsoft');
    getImpl = async () => {
      throw new Error('network down');
    };

    await expect(refreshPlayerHead(UUID_A, 'Microsoft')).resolves.toBe(false);

    expect(getPlayerSkinState(UUID_A, 'Microsoft').skin).toBe(SKIN_A);
    expect(getPlayerHeadState(UUID_A).src).toBe(SKIN_A);
  });
});

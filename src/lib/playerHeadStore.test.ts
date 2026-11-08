import { beforeEach, describe, expect, it } from 'vitest';
import {
  PLAYER_HEAD_TTL_MS,
  __playerHeadTest,
  getPlayerHeadState,
  getPlayerSkinState,
  forgetPlayerHead,
  invalidatePlayerHead,
  requestPlayerHead,
  subscribePlayerHead,
  subscribePlayerSkin,
} from './playerHeadStore';

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
    expect(getPlayerHeadState(UUID_A)).toEqual({ src: 'OLD_HEAD', status: 'ready' });
    expect(getPlayerSkinState(UUID_A)).toEqual({ skin: 'OLD_SKIN', status: 'ready' });

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

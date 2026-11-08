import { invoke as tauriInvoke } from '@tauri-apps/api/core';
import { headFromSkin } from './skinHead';

/**
 * Shared player-head state for every Avatar surface and the 3D preview.
 *
 * One backend request per UUID/generation feeds both representations: the raw
 * skin PNG for skinview3d and the cropped head for 2D avatars. Keeping one
 * cache and one generation map is what prevents the old "3D updated, 2D
 * stayed old" split.
 *
 * The explicit refresh path is deliberately ordered:
 *   1. bump the renderer generation (old in-flight responses are discarded);
 *   2. mark the key as invalidating (reads are gated until this finishes);
 *   3. delete the backend disk entry and advance the Rust generation;
 *   4. clear the renderer entries;
 *   5. notify subscribers (loading) and start one forced request;
 *   6. publish the fresh raw skin/head to all subscribers.
 *
 * A failed backend invalidation leaves the old renderer cache in place and is
 * reported to the caller, so the UI never claims success and never loses a
 * working image.
 */

export const PLAYER_HEAD_TTL_MS = 60 * 60 * 1000;

export type PlayerHeadStatus = 'ready' | 'loading' | 'failed';

export interface PlayerHeadState {
  src: string | null;
  status: PlayerHeadStatus;
}

export interface PlayerSkinState {
  skin: string | null;
  status: PlayerHeadStatus;
}

type HeadListener = (head: string | null) => void;
type SkinListener = (skin: string | null) => void;
type InvalidateListener = (key: string) => void;
type InvokeFn = (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;
type CropFn = (uuid: string, skinDataUrl: string) => Promise<string | null>;

interface TimedValue {
  value: string;
  at: number;
}

interface Inflight {
  gen: number;
  promise: Promise<string | null>;
}

const defaultInvoke: InvokeFn = (cmd, args) => tauriInvoke(cmd, args);
const defaultCrop: CropFn = headFromSkin;
const defaultNow = () => Date.now();

let invokeFn: InvokeFn = defaultInvoke;
let cropFn: CropFn = defaultCrop;
let nowFn: () => number = defaultNow;

const headCache = new Map<string, TimedValue>();
const rawSkinCache = new Map<string, TimedValue>();
const failedAt = new Map<string, number>();
const generations = new Map<string, number>();
/** Backend invalidation currently in progress; reads wait for it. */
const invalidating = new Map<string, Promise<void>>();
const inflight = new Map<string, Inflight>();
const headSubs = new Map<string, Set<HeadListener>>();
const skinSubs = new Map<string, Set<SkinListener>>();
const invalidationListeners = new Set<InvalidateListener>();

const RETRY_MS = 60_000;

/** Normalize a UUID exactly like the Rust side: lowercase, no dashes. */
export function normalizePlayerKey(uuid: string): string | null {
  const key = uuid.replace(/-/g, '').toLowerCase();
  return /^[0-9a-f]{32}$/.test(key) ? key : null;
}

function isFresh(entry: TimedValue): boolean {
  return nowFn() - entry.at < PLAYER_HEAD_TTL_MS;
}

function getGeneration(key: string): number {
  return generations.get(key) ?? 0;
}

function bumpGeneration(key: string): number {
  const next = getGeneration(key) + 1;
  generations.set(key, next);
  return next;
}

function notifyHead(key: string, head: string | null): void {
  const subs = headSubs.get(key);
  if (!subs) return;
  for (const fn of [...subs]) {
    try {
      fn(head);
    } catch (e) {
      console.error('player head subscriber failed:', e);
    }
  }
}

function notifySkin(key: string, skin: string | null): void {
  const subs = skinSubs.get(key);
  if (!subs) return;
  for (const fn of [...subs]) {
    try {
      fn(skin);
    } catch (e) {
      console.error('player skin subscriber failed:', e);
    }
  }
}

export function getPlayerHeadState(uuid: string): PlayerHeadState {
  const key = normalizePlayerKey(uuid);
  if (!key) return { src: null, status: 'failed' };
  const cached = headCache.get(key);
  if (cached) return { src: cached.value, status: 'ready' };
  const failed = failedAt.get(key);
  return {
    src: null,
    status: failed !== undefined && nowFn() - failed < RETRY_MS ? 'failed' : 'loading',
  };
}

export function getPlayerSkinState(uuid: string): PlayerSkinState {
  const key = normalizePlayerKey(uuid);
  if (!key) return { skin: null, status: 'failed' };
  const cached = rawSkinCache.get(key);
  if (cached) return { skin: cached.value, status: 'ready' };
  const failed = failedAt.get(key);
  return {
    skin: null,
    status: failed !== undefined && nowFn() - failed < RETRY_MS ? 'failed' : 'loading',
  };
}

export function subscribePlayerHead(
  uuid: string,
  listener: HeadListener,
): () => void {
  const key = normalizePlayerKey(uuid);
  if (!key) return () => {};
  let subs = headSubs.get(key);
  if (!subs) {
    subs = new Set();
    headSubs.set(key, subs);
  }
  subs.add(listener);
  return () => {
    const current = headSubs.get(key);
    if (!current) return;
    current.delete(listener);
    if (current.size === 0) headSubs.delete(key);
  };
}

export function subscribePlayerSkin(
  uuid: string,
  listener: SkinListener,
): () => void {
  const key = normalizePlayerKey(uuid);
  if (!key) return () => {};
  let subs = skinSubs.get(key);
  if (!subs) {
    subs = new Set();
    skinSubs.set(key, subs);
  }
  subs.add(listener);
  return () => {
    const current = skinSubs.get(key);
    if (!current) return;
    current.delete(listener);
    if (current.size === 0) skinSubs.delete(key);
  };
}

/**
 * Subscribe to the completed backend invalidation of one account's skin.
 * Kept for consumers that need a raw refresh signal; the shared hooks below
 * already receive the fresh values through their own subscriptions.
 */
export function onPlayerHeadInvalidated(
  listener: InvalidateListener,
): () => void {
  invalidationListeners.add(listener);
  return () => invalidationListeners.delete(listener);
}

/** Wait until no backend invalidation is pending for this key. */
async function waitForInvalidation(key: string): Promise<void> {
  for (;;) {
    const pending = invalidating.get(key);
    if (!pending) return;
    try {
      await pending;
    } catch {
      // A failed invalidation leaves the old cache in place; callers may
      // still read it. The explicit refresh caller receives the error.
      return;
    }
  }
}

function recordFailure(key: string, previousHead: string | null): void {
  failedAt.set(key, nowFn());
  // Keep a working image on an automatic TTL/fetch failure. An explicit
  // invalidation has already cleared it, so there the UI falls back to the
  // letter/loading state instead of showing known-stale pixels.
  if (!previousHead) notifyHead(key, null);
  // The 3D hook subscribes to the skin channel only; without this terminal
  // notification it would stay in `loading` forever after a failed fetch.
  if (!rawSkinCache.has(key)) notifySkin(key, null);
}

async function fetchHead(key: string, gen: number): Promise<string | null> {
  const previousHead = headCache.get(key)?.value ?? null;
  try {
    const skin = await invokeFn('cmd_get_player_head', { uuid: key });
    if (getGeneration(key) !== gen) return null;
    if (typeof skin !== 'string' || skin.length === 0) {
      recordFailure(key, previousHead);
      return null;
    }

    // Publish the raw skin immediately: the 3D preview can swap its texture
    // without waiting for the 2D crop (which needs a canvas).
    rawSkinCache.set(key, { value: skin, at: nowFn() });
    notifySkin(key, skin);

    const head = await cropFn(key, skin);
    if (getGeneration(key) !== gen) return null;
    if (!head) {
      recordFailure(key, previousHead);
      return null;
    }

    headCache.set(key, { value: head, at: nowFn() });
    failedAt.delete(key);
    notifyHead(key, head);
    return head;
  } catch {
    if (getGeneration(key) !== gen) return null;
    recordFailure(key, previousHead);
    return null;
  }
}

/**
 * Request the current skin/head for one UUID.
 *
 * A fresh renderer entry is returned without touching the network. Expired
 * entries are revalidated in the background while the old image stays
 * visible. `force` is used only by explicit invalidation and bypasses every
 * cache/failure gate, but still honours a pending backend invalidation.
 */
export async function requestPlayerHead(
  uuid: string,
  options: { force?: boolean } = {},
): Promise<string | null> {
  const key = normalizePlayerKey(uuid);
  if (!key) return null;

  await waitForInvalidation(key);
  // A new invalidation can start in the microtask between the wait above
  // returning and this continuation. Re-check synchronously before opening a
  // request, so a forced refresh never reads the old disk entry.
  while (invalidating.has(key)) {
    await waitForInvalidation(key);
  }

  const force = options.force === true;
  const gen = getGeneration(key);
  const cachedHead = headCache.get(key);
  const cachedSkin = rawSkinCache.get(key);

  if (!force) {
    // Both representations must have a fresh entry before the shared request
    // is skipped; otherwise a 3D mount could reuse only the cropped head and
    // never receive its raw skin.
    if (cachedHead && isFresh(cachedHead) && cachedSkin && isFresh(cachedSkin)) {
      return cachedHead.value;
    }
    const active = inflight.get(key);
    if (active && active.gen === gen) return active.promise;
    const failed = failedAt.get(key);
    if (failed !== undefined && nowFn() - failed < RETRY_MS) {
      // Preserve the old image during the cooldown when one exists.
      return cachedHead?.value ?? null;
    }
  } else {
    const active = inflight.get(key);
    if (active && active.gen === gen) return active.promise;
  }

  const entry: Inflight = { gen, promise: Promise.resolve(null) };
  const promise = fetchHead(key, gen).finally(() => {
    if (inflight.get(key) === entry) inflight.delete(key);
  });
  entry.promise = promise;
  inflight.set(key, entry);
  return promise;
}

/**
 * Explicitly refresh one account. Returns `true` when a fresh head was
 * produced. Throws when the backend invalidation itself fails (the old
 * renderer image is then deliberately kept).
 */
export async function invalidatePlayerHead(uuid: string): Promise<boolean> {
  const key = normalizePlayerKey(uuid);
  if (!key) return false;

  const gen = bumpGeneration(key);

  const backend = (async () => {
    await invokeFn('cmd_invalidate_player_head', { uuid: key });
  })();
  invalidating.set(key, backend);

  try {
    await backend;
  } catch (e) {
    if (invalidating.get(key) === backend) invalidating.delete(key);
    // The generation was bumped so old in-flight responses still lose, but
    // the working bytes remain visible and readable. If there were none, mark
    // both representations failed instead of leaving them in `loading`.
    const oldHead = headCache.get(key)?.value ?? null;
    const oldSkin = rawSkinCache.get(key)?.value ?? null;
    if (!oldHead && !oldSkin) failedAt.set(key, nowFn());
    notifyHead(key, oldHead);
    notifySkin(key, oldSkin);
    throw e;
  }

  if (getGeneration(key) !== gen) {
    // A newer refresh superseded this one. Let it finish and report its
    // result instead of clearing the cache twice or starting a stale request.
    if (invalidating.get(key) === backend) invalidating.delete(key);
    const newer = invalidating.get(key);
    if (newer) {
      try {
        await newer;
      } catch {
        return false;
      }
    }
    return headCache.has(key);
  }

  invalidating.delete(key);
  headCache.delete(key);
  rawSkinCache.delete(key);
  failedAt.delete(key);
  // Drop the old marker without cancelling the request: its generation check
  // discards the response, and the forced request below must not be blocked.
  inflight.delete(key);
  notifyHead(key, null);
  notifySkin(key, null);
  for (const fn of [...invalidationListeners]) {
    try {
      fn(key);
    } catch (e) {
      console.error('player head invalidation listener failed:', e);
    }
  }

  const head = await requestPlayerHead(key, { force: true });
  if (getGeneration(key) !== gen) {
    const newer = invalidating.get(key);
    if (newer) {
      try {
        await newer;
      } catch {
        return false;
      }
    }
    return headCache.has(key);
  }
  return head !== null;
}

/**
 * Drop every renderer cache entry for a removed account without starting a
 * network refresh. The backend account removal already deletes the disk head;
 * calling `invalidatePlayerHead` here would immediately re-fetch/re-cache an
 * avatar for an account that no longer exists.
 */
export function forgetPlayerHead(uuid: string): void {
  const key = normalizePlayerKey(uuid);
  if (!key) return;
  bumpGeneration(key);
  headCache.delete(key);
  rawSkinCache.delete(key);
  failedAt.delete(key);
  inflight.delete(key);
  notifyHead(key, null);
  notifySkin(key, null);
}

/** Test-only controls; never used by production code. */
export const __playerHeadTest = {
  setRuntime(next: {
    invoke?: InvokeFn;
    crop?: CropFn;
    now?: () => number;
  }): void {
    if (next.invoke) invokeFn = next.invoke;
    if (next.crop) cropFn = next.crop;
    if (next.now) nowFn = next.now;
  },

  reset(): void {
    invokeFn = defaultInvoke;
    cropFn = defaultCrop;
    nowFn = defaultNow;
    headCache.clear();
    rawSkinCache.clear();
    failedAt.clear();
    generations.clear();
    invalidating.clear();
    inflight.clear();
    headSubs.clear();
    skinSubs.clear();
    invalidationListeners.clear();
  },

  seedHead(uuid: string, head: string, at = nowFn()): void {
    const key = normalizePlayerKey(uuid);
    if (key) headCache.set(key, { value: head, at });
  },

  seedSkin(uuid: string, skin: string, at = nowFn()): void {
    const key = normalizePlayerKey(uuid);
    if (key) rawSkinCache.set(key, { value: skin, at });
  },

  head(uuid: string): string | null {
    const key = normalizePlayerKey(uuid);
    return key ? headCache.get(key)?.value ?? null : null;
  },

  skin(uuid: string): string | null {
    const key = normalizePlayerKey(uuid);
    return key ? rawSkinCache.get(key)?.value ?? null : null;
  },

  generation(uuid: string): number {
    const key = normalizePlayerKey(uuid);
    return key ? getGeneration(key) : 0;
  },

  inflightCount(): number {
    return inflight.size;
  },

  isInvalidating(uuid: string): boolean {
    const key = normalizePlayerKey(uuid);
    return key ? invalidating.has(key) : false;
  },
};

import { invoke as tauriInvoke } from '@tauri-apps/api/core';
import { headFromSkin } from './skinHead';
import { useSkinSettingsStore } from '../stores/skinSettingsStore';

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

export const PLAYER_HEAD_TTL_MS = 24 * 60 * 60 * 1000;

export type PlayerHeadStatus = 'ready' | 'loading' | 'failed';

export interface PlayerHeadState {
  src: string | null;
  status: PlayerHeadStatus;
  /** A cached head is visible while a newer skin is being fetched. */
  refreshing?: boolean;
}

export interface PlayerSkinState {
  skin: string | null;
  status: PlayerHeadStatus;
  /** A cached skin is visible while a newer one is being fetched. */
  refreshing?: boolean;
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
/** Heads currently being revalidated while a cached image stays visible. */
const refreshing = new Set<string>();
/** Backend invalidation currently in progress; reads wait for it. */
const invalidating = new Map<string, Promise<void>>();
const inflight = new Map<string, Inflight>();
const headSubs = new Map<string, Set<HeadListener>>();
const skinSubs = new Map<string, Set<SkinListener>>();
const invalidationListeners = new Set<InvalidateListener>();

const RETRY_MS = 60_000;
export const PLAYER_HEAD_STORAGE_KEY = 'voidlauncher-player-head-cache-v1';
export const PLAYER_SKIN_STORAGE_KEY = 'voidlauncher-player-skin-cache-v1';
/** Compact cropped heads only; 120 entries stay well below localStorage limits. */
const MAX_PERSISTED_HEADS = 120;
/** Full skins are larger; 60 entries keep the cache bounded. */
const MAX_PERSISTED_SKINS = 60;
const MAX_PERSISTED_AGE_MS = 30 * 24 * 60 * 60 * 1000;
const PERSISTED_HEAD_PREFIX = 'data:image/png;base64,';

/**
 * Load the cropped heads persisted by a previous launcher run. This is
 * synchronous on purpose: the first React render already has an image, so
 * the black shine never replaces a skin that was fetched successfully before.
 */
function loadPersistedHeads(): Map<string, TimedValue> {
  const loaded = new Map<string, TimedValue>();
  try {
    const raw = localStorage.getItem(PLAYER_HEAD_STORAGE_KEY);
    if (!raw) return loaded;
    const parsed = JSON.parse(raw) as Record<string, { value?: unknown; at?: unknown }>;
    for (const [key, entry] of Object.entries(parsed)) {
      if (!/^[0-9a-f]{32}$/.test(key)) continue;
      if (!entry || typeof entry.value !== 'string') continue;
      if (!entry.value.startsWith(PERSISTED_HEAD_PREFIX)) continue;
      const at = typeof entry.at === 'number' && Number.isFinite(entry.at) ? entry.at : 0;
      // Drop ancient entries instead of growing the cache forever. Recent
      // stale entries are kept: they are the offline fallback.
      if (at > 0 && Date.now() - at > MAX_PERSISTED_AGE_MS) continue;
      loaded.set(key, { value: entry.value, at });
    }
  } catch {
    // Corrupt or unavailable storage: start with no persisted heads.
  }
  return loaded;
}

function persistHeads(): void {
  try {
    const entries = [...headCache.entries()]
      .filter(([, entry]) => entry.value.startsWith(PERSISTED_HEAD_PREFIX))
      .sort((a, b) => b[1].at - a[1].at)
      .slice(0, MAX_PERSISTED_HEADS);
    const payload: Record<string, TimedValue> = {};
    for (const [key, entry] of entries) payload[key] = entry;
    localStorage.setItem(PLAYER_HEAD_STORAGE_KEY, JSON.stringify(payload));
  } catch {
    // Quota/private mode: the in-memory cache still works for this session.
  }
}

function pruneHeadCache(): void {
  if (headCache.size <= MAX_PERSISTED_HEADS) return;
  const oldest = [...headCache.entries()]
    .sort((a, b) => a[1].at - b[1].at)
    .slice(0, headCache.size - MAX_PERSISTED_HEADS);
  for (const [key] of oldest) headCache.delete(key);
}

// Hydrate before any component subscribes.
for (const [key, entry] of loadPersistedHeads()) headCache.set(key, entry);

function normalizeProvider(provider?: string): string {
  return (provider || 'unknown').trim().toLowerCase() || 'unknown';
}

/** Cache key for the full skin: provider and account UUID are both required. */
export function playerSkinCacheKey(uuid: string, provider?: string): string | null {
  const key = normalizePlayerKey(uuid);
  return key ? `${normalizeProvider(provider)}:${key}` : null;
}

function loadPersistedSkins(): Map<string, TimedValue> {
  const loaded = new Map<string, TimedValue>();
  try {
    const raw = localStorage.getItem(PLAYER_SKIN_STORAGE_KEY);
    if (!raw) return loaded;
    const parsed = JSON.parse(raw) as Record<string, { value?: unknown; at?: unknown }>;
    for (const [key, entry] of Object.entries(parsed)) {
      if (!/^[a-z0-9_-]+:[0-9a-f]{32}$/.test(key)) continue;
      if (!entry || typeof entry.value !== 'string') continue;
      if (!entry.value.startsWith(PERSISTED_HEAD_PREFIX)) continue;
      const at = typeof entry.at === 'number' && Number.isFinite(entry.at) ? entry.at : 0;
      if (at > 0 && Date.now() - at > MAX_PERSISTED_AGE_MS) continue;
      loaded.set(key, { value: entry.value, at });
    }
  } catch {
    // Corrupt or unavailable storage: no persisted full skins.
  }
  return loaded;
}

function persistSkins(): void {
  try {
    const entries = [...rawSkinCache.entries()]
      .filter(([, entry]) => entry.value.startsWith(PERSISTED_HEAD_PREFIX))
      .sort((a, b) => b[1].at - a[1].at)
      .slice(0, MAX_PERSISTED_SKINS);
    const payload: Record<string, TimedValue> = {};
    for (const [key, entry] of entries) payload[key] = entry;
    localStorage.setItem(PLAYER_SKIN_STORAGE_KEY, JSON.stringify(payload));
  } catch {
    // Quota/private mode: the in-memory skin cache still works.
  }
}

function pruneSkinCache(): void {
  if (rawSkinCache.size <= MAX_PERSISTED_SKINS) return;
  const oldest = [...rawSkinCache.entries()]
    .sort((a, b) => a[1].at - b[1].at)
    .slice(0, rawSkinCache.size - MAX_PERSISTED_SKINS);
  for (const [key] of oldest) rawSkinCache.delete(key);
}

for (const [key, entry] of loadPersistedSkins()) rawSkinCache.set(key, entry);

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
  if (cached) {
    return refreshing.has(key)
      ? { src: cached.value, status: 'ready', refreshing: true }
      : { src: cached.value, status: 'ready' };
  }
  const failed = failedAt.get(key);
  return {
    src: null,
    status: failed !== undefined && nowFn() - failed < RETRY_MS ? 'failed' : 'loading',
  };
}

export function getPlayerSkinState(
  uuid: string,
  provider?: string,
): PlayerSkinState {
  const key = playerSkinCacheKey(uuid, provider);
  if (!key) return { skin: null, status: 'failed' };
  const cached = rawSkinCache.get(key);
  if (cached) {
    return refreshing.has(normalizePlayerKey(uuid) ?? key)
      ? { skin: cached.value, status: 'ready', refreshing: true }
      : { skin: cached.value, status: 'ready' };
  }
  const bare = normalizePlayerKey(uuid);
  const failed = bare ? failedAt.get(bare) : undefined;
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
  provider?: string,
): () => void {
  const key = playerSkinCacheKey(uuid, provider);
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

function recordFailure(
  headKey: string,
  skinKey: string,
  previousHead: string | null,
): void {
  failedAt.set(headKey, nowFn());
  refreshing.delete(headKey);
  // Keep a working image on an automatic TTL/fetch failure. An explicit
  // invalidation has already cleared it, so there the UI falls back to the
  // letter/loading state instead of showing known-stale pixels. Notify even
  // with the same value so the overlay/spinner stops.
  notifyHead(headKey, previousHead);
  // Notify the 3D channel with its existing value (or null when nothing is
  // cached) so the small background-refresh indicator also stops.
  notifySkin(skinKey, rawSkinCache.get(skinKey)?.value ?? null);
}

async function fetchHead(
  headKey: string,
  skinKey: string,
  gen: number,
  force: boolean,
): Promise<string | null> {
  const previousHead = headCache.get(headKey)?.value ?? null;
  const previousSkin = rawSkinCache.get(skinKey)?.value ?? null;
  if (previousHead || previousSkin) {
    // Stale-while-revalidate: keep the last good image on screen and let the
    // Avatar layer draw the shine over it until this attempt resolves.
    refreshing.add(headKey);
    notifyHead(headKey, previousHead);
    notifySkin(skinKey, previousSkin);
  }
  try {
    const skin = await invokeFn('cmd_get_player_head', { uuid: headKey, force });
    if (getGeneration(headKey) !== gen) {
      refreshing.delete(headKey);
      return null;
    }
    if (typeof skin !== 'string' || skin.length === 0) {
      recordFailure(headKey, skinKey, previousHead);
      return null;
    }

    // Publish the raw skin immediately: the 3D preview can swap its texture
    // without waiting for the 2D crop (which needs a canvas).
    rawSkinCache.set(skinKey, { value: skin, at: nowFn() });
    pruneSkinCache();
    persistSkins();
    notifySkin(skinKey, skin);

    const head = await cropFn(headKey, skin);
    if (getGeneration(headKey) !== gen) {
      refreshing.delete(headKey);
      return null;
    }
    if (!head) {
      recordFailure(headKey, skinKey, previousHead);
      return null;
    }

    headCache.set(headKey, { value: head, at: nowFn() });
    pruneHeadCache();
    persistHeads();
    failedAt.delete(headKey);
    refreshing.delete(headKey);
    notifyHead(headKey, head);
    // Refresh the 3D subscriber as well so its small background indicator
    // stops even though the texture value itself did not change.
    notifySkin(skinKey, skin);
    return head;
  } catch {
    if (getGeneration(headKey) !== gen) {
      refreshing.delete(headKey);
      return null;
    }
    recordFailure(headKey, skinKey, previousHead);
    return null;
  }
}

/** Build the cropped head from an already cached full texture (no network). */
async function headFromCachedSkin(
  headKey: string,
  skin: string,
  gen: number,
): Promise<string | null> {
  const head = await cropFn(headKey, skin);
  if (getGeneration(headKey) !== gen) return null;
  if (!head) return null;
  headCache.set(headKey, { value: head, at: nowFn() });
  pruneHeadCache();
  persistHeads();
  failedAt.delete(headKey);
  notifyHead(headKey, head);
  return head;
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
  options: { force?: boolean; provider?: string } = {},
): Promise<string | null> {
  const headKey = normalizePlayerKey(uuid);
  const skinKey = playerSkinCacheKey(uuid, options.provider);
  if (!headKey || !skinKey) return null;

  await waitForInvalidation(headKey);
  // A new invalidation can start in the microtask between the wait above
  // returning and this continuation. Re-check synchronously before opening a
  // request, so a forced refresh never reads the old disk entry.
  while (invalidating.has(headKey)) {
    await waitForInvalidation(headKey);
  }

  const force = options.force === true;
  const gen = getGeneration(headKey);
  const cachedHead = headCache.get(headKey);
  const cachedSkin = rawSkinCache.get(skinKey);
  const manual = useSkinSettingsStore.getState().refreshMode === 'manual';

  if (!force) {
    // Manual mode never refreshes a skin that is already cached. The first
    // full-skin load is still allowed when nothing is available, otherwise a
    // new account could never show a model.
    if (manual && cachedSkin) {
      if (cachedHead) return cachedHead.value;
      return headFromCachedSkin(headKey, cachedSkin.value, gen);
    }
    // A fresh full texture can also fill a missing/stale head locally; a
    // network round-trip only for the small avatar would be wasteful.
    if (cachedSkin && isFresh(cachedSkin)) {
      if (cachedHead && isFresh(cachedHead)) return cachedHead.value;
      const derived = await headFromCachedSkin(headKey, cachedSkin.value, gen);
      if (derived) return derived;
    }
    // Both representations must have a fresh entry before the shared request
    // is skipped; otherwise a 3D mount could reuse only the cropped head and
    // never receive its raw skin.
    if (cachedHead && isFresh(cachedHead) && cachedSkin && isFresh(cachedSkin)) {
      return cachedHead.value;
    }
    const active = inflight.get(skinKey);
    if (active && active.gen === gen) return active.promise;
    const failed = failedAt.get(headKey);
    if (failed !== undefined && nowFn() - failed < RETRY_MS) {
      // Preserve the old image during the cooldown when one exists.
      return cachedHead?.value ?? null;
    }
  } else {
    const active = inflight.get(skinKey);
    if (active && active.gen === gen) return active.promise;
  }

  const entry: Inflight = { gen, promise: Promise.resolve(null) };
  const promise = fetchHead(headKey, skinKey, gen, force).finally(() => {
    if (inflight.get(skinKey) === entry) inflight.delete(skinKey);
  });
  entry.promise = promise;
  inflight.set(skinKey, entry);
  return promise;
}

/**
 * Explicit manual refresh: fetch the current texture without deleting the
 * working cache first. The old image stays on screen while the request runs,
 * and a network failure leaves it untouched.
 */
export async function refreshPlayerHead(
  uuid: string,
  provider?: string,
): Promise<boolean> {
  const headKey = normalizePlayerKey(uuid);
  if (!headKey) return false;
  const gen = bumpGeneration(headKey);
  const head = await requestPlayerHead(headKey, { force: true, provider });
  if (getGeneration(headKey) !== gen) return false;
  return head !== null;
}

/**
 * Explicitly refresh one account. Returns `true` when a fresh head was
 * produced. Throws when the backend invalidation itself fails (the old
 * renderer image is then deliberately kept).
 */
export async function invalidatePlayerHead(
  uuid: string,
  provider?: string,
): Promise<boolean> {
  const key = normalizePlayerKey(uuid);
  const skinKey = playerSkinCacheKey(uuid, provider);
  if (!key || !skinKey) return false;

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
    const oldSkin = rawSkinCache.get(skinKey)?.value ?? null;
    refreshing.delete(key);
    if (!oldHead && !oldSkin) failedAt.set(key, nowFn());
    notifyHead(key, oldHead);
    notifySkin(skinKey, oldSkin);
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
  rawSkinCache.delete(skinKey);
  failedAt.delete(key);
  refreshing.delete(key);
  persistHeads();
  persistSkins();
  // Drop the old marker without cancelling the request: its generation check
  // discards the response, and the forced request below must not be blocked.
  inflight.delete(skinKey);
  notifyHead(key, null);
  notifySkin(skinKey, null);
  for (const fn of [...invalidationListeners]) {
    try {
      fn(key);
    } catch (e) {
      console.error('player head invalidation listener failed:', e);
    }
  }

  const head = await requestPlayerHead(key, { force: true, provider });
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
export function forgetPlayerHead(uuid: string, provider?: string): void {
  const key = normalizePlayerKey(uuid);
  const skinKey = playerSkinCacheKey(uuid, provider);
  if (!key || !skinKey) return;
  bumpGeneration(key);
  headCache.delete(key);
  rawSkinCache.delete(skinKey);
  failedAt.delete(key);
  refreshing.delete(key);
  inflight.delete(skinKey);
  persistHeads();
  persistSkins();
  notifyHead(key, null);
  notifySkin(skinKey, null);
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
    refreshing.clear();
    invalidating.clear();
    inflight.clear();
    headSubs.clear();
    skinSubs.clear();
    invalidationListeners.clear();
    try {
      localStorage.removeItem(PLAYER_HEAD_STORAGE_KEY);
      localStorage.removeItem(PLAYER_SKIN_STORAGE_KEY);
    } catch {
      // Storage unavailable: nothing to clear.
    }
  },

  seedHead(uuid: string, head: string, at = nowFn()): void {
    const key = normalizePlayerKey(uuid);
    if (key) headCache.set(key, { value: head, at });
  },

  seedSkin(uuid: string, skin: string, at = nowFn(), provider?: string): void {
    const key = playerSkinCacheKey(uuid, provider);
    if (key) rawSkinCache.set(key, { value: skin, at });
  },

  head(uuid: string): string | null {
    const key = normalizePlayerKey(uuid);
    return key ? headCache.get(key)?.value ?? null : null;
  },

  skin(uuid: string, provider?: string): string | null {
    const key = playerSkinCacheKey(uuid, provider);
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

  persistedHead(uuid: string): string | null {
    try {
      const raw = localStorage.getItem(PLAYER_HEAD_STORAGE_KEY);
      if (!raw) return null;
      const parsed = JSON.parse(raw) as Record<string, { value?: unknown }>;
      const key = normalizePlayerKey(uuid);
      const value = key ? parsed[key]?.value : null;
      return typeof value === 'string' ? value : null;
    } catch {
      return null;
    }
  },

  persistedSkin(uuid: string, provider?: string): string | null {
    try {
      const raw = localStorage.getItem(PLAYER_SKIN_STORAGE_KEY);
      if (!raw) return null;
      const parsed = JSON.parse(raw) as Record<string, { value?: unknown }>;
      const key = playerSkinCacheKey(uuid, provider);
      const value = key ? parsed[key]?.value : null;
      return typeof value === 'string' ? value : null;
    } catch {
      return null;
    }
  },

  /** Simulate an app restart: drop memory and re-hydrate from localStorage. */
  reloadPersisted(): void {
    headCache.clear();
    rawSkinCache.clear();
    failedAt.clear();
    refreshing.clear();
    for (const [key, entry] of loadPersistedHeads()) headCache.set(key, entry);
    for (const [key, entry] of loadPersistedSkins()) rawSkinCache.set(key, entry);
  },

  isRefreshing(uuid: string): boolean {
    const key = normalizePlayerKey(uuid);
    return key ? refreshing.has(key) : false;
  },
};

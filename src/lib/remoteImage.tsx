import { useCallback, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { headFromSkin } from './skinHead';

// ---- Remote images via cmd_fetch_icon_url -----------------------------------
// CDN images (mod icon, thumbnails) are never handed to
// <img src="https://..."> directly: the webview follows only the system proxy
// with no proxy→direct fallback, so a single flaky host leaves every image
// blank for some users. Each image is fetched through cmd_fetch_icon_url
// (send_with_fallback: proxy → direct) and rendered as a data URL; session
// RAM cache + retry cooldown keep re-renders cheap. The Rust side restricts
// hosts to an allowlist so the renderer cannot use this as an SSRF proxy.
// Player-head avatars use a separate pipeline (cmd_get_player_head), see
// the Avatar section below.
const remoteImageCache = new Map<string, string>();
const remoteImageFailedAt = new Map<string, number>();
const remoteImageInflight = new Set<string>();
const remoteImageSubs = new Map<string, ((v: string | null) => void)[]>();
const REMOTE_IMAGE_RETRY_MS = 60_000;

function requestRemoteImage(url: string) {
  if (remoteImageInflight.has(url)) return;
  const failedAt = remoteImageFailedAt.get(url);
  if (failedAt && Date.now() - failedAt < REMOTE_IMAGE_RETRY_MS) {
    setTimeout(() => {
      (remoteImageSubs.get(url) ?? []).forEach((fn) => fn(null));
      remoteImageSubs.delete(url);
    }, 0);
    return;
  }
  remoteImageInflight.add(url);
  invoke<string | null>('cmd_fetch_icon_url', { url })
    .then((data) => {
      if (data) {
        remoteImageCache.set(url, data);
        remoteImageFailedAt.delete(url);
      } else {
        remoteImageFailedAt.set(url, Date.now());
      }
      (remoteImageSubs.get(url) ?? []).forEach((fn) => fn(data ?? null));
    })
    .catch(() => {
      remoteImageFailedAt.set(url, Date.now());
      (remoteImageSubs.get(url) ?? []).forEach((fn) => fn(null));
    })
    .finally(() => {
      remoteImageInflight.delete(url);
      remoteImageSubs.delete(url);
    });
}

export type RemoteStatus = 'ready' | 'loading' | 'failed';

type FetchedState = { url: string; value: string | null };

/**
 * Shared remote-image state machine:
 *   ready   — data URL is available (cache hit, data:, or successful fetch);
 *   loading — request in flight → callers show a shine placeholder;
 *   failed  — no image (missing url, fetch error, cooldown) → letter fallback.
 */
export function useRemoteImage(url: string | null | undefined): {
  src: string | null;
  status: RemoteStatus;
  onError: () => void;
} {
  const [fetched, setFetched] = useState<FetchedState | null>(null);

  useEffect(() => {
    if (!url || url.startsWith('data:')) return;

    const cached = remoteImageCache.get(url);
    if (cached) {
      setFetched({ url, value: cached });
      return;
    }

    const failedAt = remoteImageFailedAt.get(url);
    if (failedAt && Date.now() - failedAt < REMOTE_IMAGE_RETRY_MS) {
      setFetched({ url, value: null });
      return;
    }

    let alive = true;
    const subs = remoteImageSubs.get(url) ?? [];
    subs.push((v) => {
      if (alive) setFetched({ url, value: v });
    });
    remoteImageSubs.set(url, subs);
    requestRemoteImage(url);
    return () => {
      alive = false;
    };
  }, [url]);

  const onError = useCallback(() => {
    if (url && !url.startsWith('data:')) {
      remoteImageFailedAt.set(url, Date.now());
    }
    if (url) setFetched({ url, value: null });
  }, [url]);

  let src: string | null = null;
  let status: RemoteStatus;

  if (!url) {
    status = 'failed';
  } else {
    const f = fetched && fetched.url === url ? fetched : null;
    if (f) {
      if (f.value) {
        src = f.value;
        status = 'ready';
      } else {
        status = 'failed';
      }
    } else if (url.startsWith('data:')) {
      src = url;
      status = 'ready';
    } else {
      const cached = remoteImageCache.get(url);
      if (cached) {
        src = cached;
        status = 'ready';
      } else {
        const failedAt = remoteImageFailedAt.get(url);
        status =
          failedAt && Date.now() - failedAt < REMOTE_IMAGE_RETRY_MS
            ? 'failed'
            : 'loading';
      }
    }
  }

  return { src, status, onError };
}

/** Soft light sweep shown while a remote image is loading. */
export function IconShine({
  size,
  radius,
  style,
  className,
}: {
  size?: number;
  radius?: number | string;
  style?: React.CSSProperties;
  className?: string;
}) {
  const box: React.CSSProperties = {
    position: 'relative',
    flexShrink: 0,
    overflow: 'hidden',
    ...(size != null ? { width: size, height: size } : null),
    ...(radius != null ? { borderRadius: radius } : null),
    ...style,
  };
  return (
    <div
      className={className ? `icon-shine ${className}` : 'icon-shine'}
      style={box}
      aria-hidden
    />
  );
}

export function RemoteImage({
  url,
  alt = '',
  style,
  className,
}: {
  url: string;
  alt?: string;
  style?: React.CSSProperties;
  className?: string;
}) {
  const { src, status, onError } = useRemoteImage(url);

  if (status === 'loading') {
    return <IconShine style={style} className={className} />;
  }
  if (status !== 'ready' || !src) return null;
  return (
    <img
      src={src}
      alt={alt}
      style={style}
      className={className}
      loading="lazy"
      onError={onError}
    />
  );
}

// ---- Player-head avatar -------------------------------------------------------
// Heads go through cmd_get_player_head: the backend fetches the RAW SKIN
// from Mojang (with Ely.by / mc-heads fallbacks) and caches it on disk per
// uuid; the head is cropped here in the renderer. One cached head per uuid
// is shared by every page (Home, Settings, Sidebar, Accounts), so the same
// account can never show different faces on different screens. mc-heads'
// /avatar endpoint is deliberately NOT used: it silently substitutes a
// default Steve head when its own Mojang lookup fails.

const playerHeadCache = new Map<string, string>();
const playerHeadFailedAt = new Map<string, number>();
const playerHeadInflight = new Map<string, number>();
const playerHeadSubs = new Map<string, ((v: string | null) => void)[]>();
const playerHeadListeners = new Set<(key: string, head: string | null) => void>();
const playerHeadRefreshListeners = new Set<(key: string) => void>();
const playerHeadGenerations = new Map<string, number>();
const PLAYER_HEAD_RETRY_MS = 60_000;

function getHeadGeneration(key: string): number {
  return playerHeadGenerations.get(key) ?? 0;
}

function bumpHeadGeneration(key: string): number {
  const next = getHeadGeneration(key) + 1;
  playerHeadGenerations.set(key, next);
  return next;
}

/**
 * Invalidate cached player head in renderer memory and on backend disk.
 *
 * All mounted Avatar instances for this UUID immediately re-fetch and re-render.
 * Advances the frontend Generation Token so in-flight requests cannot overwrite fresh state.
 */
/**
 * Subscribe to completed backend head invalidations. Used by views that need
 * the raw skin (e.g. the 3D preview) rather than the cropped Avatar head.
 */
export function onPlayerHeadInvalidated(
  listener: (key: string) => void,
): () => void {
  playerHeadRefreshListeners.add(listener);
  return () => playerHeadRefreshListeners.delete(listener);
}

export async function invalidatePlayerHead(uuid: string): Promise<void> {
  const key = uuid.replace(/-/g, '').toLowerCase();

  // 1. Advance generation token so any in-flight fetch is discarded
  bumpHeadGeneration(key);

  // 2. Clear all cached state and inflight markers for this key
  playerHeadCache.delete(key);
  playerHeadFailedAt.delete(key);
  playerHeadInflight.delete(key);
  playerHeadSubs.delete(key);

  // 3. Notify listeners that head is invalidated
  playerHeadListeners.forEach((fn) => fn(key, null));

  // 4. Invalidate on backend (disk + Rust generation token)
  try {
    await invoke('cmd_invalidate_player_head', { uuid: key });
    // Raw-skin consumers (3D preview) must refetch after the backend cache
    // is gone, otherwise they keep the old texture until remount.
    playerHeadRefreshListeners.forEach((fn) => fn(key));
  } catch (e) {
    console.warn('Failed to invalidate player head on backend:', e);
  }

  // 5. Trigger fresh request under the new generation
  requestPlayerHead(key);
}

function requestPlayerHead(uuid: string) {
  const gen = getHeadGeneration(uuid);
  if (playerHeadInflight.get(uuid) === gen) return;

  const failedAt = playerHeadFailedAt.get(uuid);
  if (failedAt && Date.now() - failedAt < PLAYER_HEAD_RETRY_MS) {
    setTimeout(() => {
      if (getHeadGeneration(uuid) === gen) {
        (playerHeadSubs.get(uuid) ?? []).forEach((fn) => fn(null));
        playerHeadSubs.delete(uuid);
      }
    }, 0);
    return;
  }

  playerHeadInflight.set(uuid, gen);
  invoke<string | null>('cmd_get_player_head', { uuid })
    .then(async (skin) => {
      // Discard if invalidated while awaiting backend
      if (getHeadGeneration(uuid) !== gen) return;

      const head = skin ? await headFromSkin(uuid, skin) : null;
      // Discard if invalidated while cropping head
      if (getHeadGeneration(uuid) !== gen) return;

      if (head) {
        playerHeadCache.set(uuid, head);
        playerHeadFailedAt.delete(uuid);
      } else {
        playerHeadFailedAt.set(uuid, Date.now());
      }
      (playerHeadSubs.get(uuid) ?? []).forEach((fn) => fn(head));
      playerHeadListeners.forEach((fn) => fn(uuid, head));
    })
    .catch(() => {
      if (getHeadGeneration(uuid) !== gen) return;
      playerHeadFailedAt.set(uuid, Date.now());
      (playerHeadSubs.get(uuid) ?? []).forEach((fn) => fn(null));
    })
    .finally(() => {
      if (playerHeadInflight.get(uuid) === gen) {
        playerHeadInflight.delete(uuid);
      }
      if (getHeadGeneration(uuid) === gen) {
        playerHeadSubs.delete(uuid);
      }
    });
}

function usePlayerHead(uuid: string | null | undefined): {
  src: string | null;
  status: 'ready' | 'loading' | 'failed';
} {
  const key = uuid ? uuid.replace(/-/g, '').toLowerCase() : null;
  const [value, setValue] = useState<{ key: string; src: string | null } | null>(null);

  useEffect(() => {
    if (!key) return;

    const cached = playerHeadCache.get(key);
    if (cached) {
      setValue({ key, src: cached });
      return;
    }

    const failedAt = playerHeadFailedAt.get(key);
    if (failedAt && Date.now() - failedAt < PLAYER_HEAD_RETRY_MS) {
      setValue({ key, src: null });
      return;
    }

    let alive = true;
    const subs = playerHeadSubs.get(key) ?? [];
    subs.push((v) => {
      if (alive) setValue({ key, src: v });
    });
    playerHeadSubs.set(key, subs);

    const onHeadEvent = (changedKey: string, newHead: string | null) => {
      if (alive && changedKey === key) {
        setValue({ key, src: newHead });
      }
    };
    playerHeadListeners.add(onHeadEvent);

    requestPlayerHead(key);
    return () => {
      alive = false;
      playerHeadListeners.delete(onHeadEvent);
    };
  }, [key]);

  if (!key) {
    return { src: null, status: 'failed' };
  }
  const f = value && value.key === key ? value : null;
  if (f) {
    return f.src
      ? { src: f.src, status: 'ready' }
      : { src: null, status: 'failed' };
  }
  const cached = playerHeadCache.get(key);
  if (cached) return { src: cached, status: 'ready' };
  const failedAt = playerHeadFailedAt.get(key);
  return {
    src: null,
    status:
      failedAt && Date.now() - failedAt < PLAYER_HEAD_RETRY_MS ? 'failed' : 'loading',
  };
}

export function Avatar({
  uuid,
  name,
  size = 40,
  style,
  className,
  radius = 'var(--radius-md)',
}: {
  uuid: string;
  name: string;
  size?: number;
  style?: React.CSSProperties;
  className?: string;
  radius?: string;
}) {
  const letter = (name?.trim().charAt(0) || '?').toUpperCase();
  const { src, status } = usePlayerHead(uuid);

  const containerStyle: React.CSSProperties = {
    width: size,
    height: size,
    flexShrink: 0,
  };
  if (style) Object.assign(containerStyle, style);

  if (status === 'loading') {
    return (
      <IconShine
        size={size}
        radius={radius}
        style={style}
        className={className}
      />
    );
  }

  if (status === 'ready' && src) {
    return (
      <img
        src={src}
        alt={name}
        className={className}
        style={{ ...containerStyle, borderRadius: radius, imageRendering: 'pixelated' }}
        loading="lazy"
      />
    );
  }

  // dominant-baseline="central" puts the glyph's midline ON the y coordinate,
  // so y = size/2 is the true vertical centre (y = 0.62·size with central
  // baseline pushed the letter noticeably below the middle).
  const letterSvg = `data:image/svg+xml,<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="0 0 ${size} ${size}"><text x="${size / 2}" y="${size / 2}" text-anchor="middle" fill="%23999999" font-family="system-ui,-apple-system,sans-serif" font-weight="600" font-size="${size * 0.42}" dominant-baseline="central">${encodeURIComponent(letter)}</text></svg>`;
  return (
    <img
      src={letterSvg}
      alt={name}
      className={className}
      style={{ ...containerStyle, borderRadius: radius }}
    />
  );
}

// ---- Offline accounts ---------------------------------------------------------
// Offline accounts used to render a fixed grayscale Steve. Now that the
// backend resolves every profile uuid — including offline ones — to the
// bundled vanilla default skin the game itself would show (see
// src-tauri/src/default_skin.rs), they go through the same Avatar pipeline
// as licensed accounts and every surface stays consistent.

// ---- Content/list icon with transparent letter fallback ----------------------
// Square icon on the shared remote pipeline: shine while loading,
// first letter of the name only when there is no image.

export function RemoteIcon({
  url,
  letter,
  size,
  radius = 4,
  fontSize,
  pending = false,
  style,
  className,
}: {
  url?: string | null;
  letter: string;
  size: number;
  radius?: number;
  fontSize?: number;
  /** Still resolving a local/project icon (no URL yet) → shine, not letter. */
  pending?: boolean;
  style?: React.CSSProperties;
  className?: string;
}) {
  const { src, status, onError } = useRemoteImage(url);

  const box: React.CSSProperties = {
    position: 'relative',
    width: size,
    height: size,
    flexShrink: 0,
    borderRadius: radius,
    ...style,
  };

  if (status === 'loading' || (pending && status !== 'ready')) {
    return (
      <IconShine
        size={size}
        radius={radius}
        style={style}
        className={className}
      />
    );
  }

  if (status === 'ready' && src) {
    return (
      <div className={className} style={box}>
        <img
          src={src}
          alt=""
          loading="lazy"
          onError={onError}
          style={{
            position: 'absolute',
            inset: 0,
            width: '100%',
            height: '100%',
            borderRadius: radius,
            objectFit: 'cover',
          }}
        />
      </div>
    );
  }

  return (
    <div className={className} style={box}>
      <div
        style={{
          position: 'absolute',
          inset: 0,
          display: 'flex',
          alignItems: 'center',
          justifyContent: 'center',
          fontSize: fontSize ?? Math.round(size * 0.4),
          color: 'var(--text-tertiary)',
          borderRadius: radius,
          overflow: 'hidden',
        }}
      >
        {letter}
      </div>
    </div>
  );
}

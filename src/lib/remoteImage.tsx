import { useCallback, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';

// ---- Remote images via cmd_fetch_icon_url -----------------------------------
// CDN images (mod icon, player-head avatars) are never handed to
// <img src="https://..."> directly: the webview follows only the system proxy
// with no proxy→direct fallback, so a single flaky host leaves every image
// blank for some users. Each image is fetched through cmd_fetch_icon_url
// (send_with_fallback: proxy → direct) and rendered as a data URL; session
// RAM cache + retry cooldown keep re-renders cheap. The Rust side restricts
// hosts to an allowlist so the renderer cannot use this as an SSRF proxy.
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
// Same remote pipeline as RemoteImage: shine while loading, deterministic
// letter fallback only when the fetch fails (never a gray hole).

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
  const url = `https://mc-heads.net/avatar/${uuid}/${size}`;
  const letter = (name?.trim().charAt(0) || '?').toUpperCase();
  const { src, status, onError } = useRemoteImage(url);

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
        style={{ ...containerStyle, borderRadius: radius, objectFit: 'cover' }}
        loading="lazy"
        onError={onError}
      />
    );
  }

  const letterSvg = `data:image/svg+xml,<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="0 0 ${size} ${size}"><text x="${size / 2}" y="${size * 0.62}" text-anchor="middle" fill="%23999999" font-family="system-ui,-apple-system,sans-serif" font-weight="600" font-size="${size * 0.42}" dominant-baseline="central">${encodeURIComponent(letter)}</text></svg>`;
  return (
    <img
      src={letterSvg}
      alt={name}
      className={className}
      style={{ ...containerStyle, borderRadius: radius }}
    />
  );
}

// ---- Offline account head (grayscale default Steve) --------------------------
// Offline accounts have no Mojang profile: mc-heads would return an
// unknown/placeholder head. Instead we embed the standard Steve face,
// desaturated to grayscale (same look Prism Launcher uses for offline).

const OFFLINE_STEVE_PNG =
  'data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAEAAAABACAYAAACqaXHeAAAAAXNSR0IArs4c6QAAAARnQU1BAACxjwv8YQUAAAAJcEhZcwAADsMAAA7DAcdvqGQAAAJQSURBVHhe7dG9zqlREIBR1yGCKOjcCEKpIVqiUYhGp/HTSERE60Z9/SrmzQ5xijM7Wd1+t3lGrdPpvCPtdjvkfTWbzVC9Xg81Go2Q86jVaoVqDiwflPdlsAyWwXIeGaxcgAPLB+V9GSyDZbCcRwYrF+DA8kF5XwbLYBks55HBygU4sHxQ3pfBMlgGy3lksGp+8G3dbjfU7/c/4u/JhSsX4AffZrAMKuXvyWDlAvzg2wyWQaX8PRmsXIAffJvBMqiUvyeDlQvwA91ut9Dz+Qy9Xq/Q4/EI+Z4Oh0Oo1+uFcgEGy2A5kAyWwfI9GSyDlQswWAbLgWSwDJbvyWAZrFyAwTJYDiSDZbB8TwbLYOUCjsfjO3K9XkPe1+VyCe33+9D9fg9tt9uP5AIcWAbL+zJYBstgGVQqF+DAMljel8EyWAbLoFK5AAeWwfK+DJbBMlgGlcoFGKSqMxwOQ+fzOWSQfE9Vxz9EuQCDVXUcSAbLYPmeqo7BygUYrKrjQDJYBsv3VHUMVi7AYFUdB5LBMli+p6pjsHIBm83mHTmdTiEXpsFg8BEXot1uF5rNZqFcgMEyWAbLoFIGy2AZrFyAwTJYBsugUgbLYBmsXIDBMlgGy6BSBstgGaxcwGq1ekfW63VoMpn8U8vlMjSfz0O5AINlsBzo1wyWwcoFGCyD5UC/ZrAMVi7AYBksB/o1g2WwcgHj8fgdGY1GIR8sNZ1OQ4vF4iO+p1yAwTJYBpVyIBlUyveUCzBYBsugUg4kg0r5nnIBBstgGVTKgWRQKd/Tf7+AP6y08fAFyS2jAAAAAElFTkSuQmCC';

export function OfflineAvatar({
  size = 40,
  style,
  className,
  radius = 'var(--radius-md)',
}: {
  size?: number;
  style?: React.CSSProperties;
  className?: string;
  radius?: string;
}) {
  const box: React.CSSProperties = {
    width: size,
    height: size,
    flexShrink: 0,
    borderRadius: radius,
    objectFit: 'cover',
    imageRendering: 'pixelated',
    ...style,
  };
  return (
    <img
      src={OFFLINE_STEVE_PNG}
      alt="Steve"
      className={className}
      style={box}
    />
  );
}

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

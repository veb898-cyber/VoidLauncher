import { useEffect, useState } from 'react';
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
function getCachedRemoteImage(url: string) {
  return url.startsWith('data:') ? url : remoteImageCache.get(url) ?? null;
}

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
  const [src, setSrc] = useState<string | null>(() => getCachedRemoteImage(url));

  useEffect(() => {
    if (url.startsWith('data:')) {
      setSrc(url);
      return;
    }
    let alive = true;
    setSrc(remoteImageCache.get(url) ?? null);
    if (!remoteImageCache.has(url)) {
      const subs = remoteImageSubs.get(url) ?? [];
      subs.push((v) => { if (alive) setSrc(v); });
      remoteImageSubs.set(url, subs);
      requestRemoteImage(url);
    }
    return () => { alive = false; };
  }, [url]);

  if (!src) return null;
  return (
    <img
      src={src}
      alt={alt}
      style={style}
      className={className}
      loading="lazy"
      onError={() => setSrc(null)}
    />
  );
}

// ---- Player-head avatar -------------------------------------------------------
// Same remote pipeline as RemoteImage, with a deterministic letter fallback so
// a missing/failed avatar never leaves an empty hole: the first letter is
// rendered as an inline SVG data URL (always available, no network).

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

  const [src, setSrc] = useState<string | null>(() => getCachedRemoteImage(url));

  useEffect(() => {
    let alive = true;
    setSrc(getCachedRemoteImage(url));
    if (!remoteImageCache.has(url)) {
      const subs = remoteImageSubs.get(url) ?? [];
      subs.push((v) => { if (alive) setSrc(v); });
      remoteImageSubs.set(url, subs);
      requestRemoteImage(url);
    }
    return () => { alive = false; };
  }, [url]);

  const letterSvg = `data:image/svg+xml,<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="0 0 ${size} ${size}"><rect fill="%23333" width="${size}" height="${size}" rx="10"/><text x="${size / 2}" y="${size * 0.62}" text-anchor="middle" fill="%23999" font-size="${size * 0.4}">${encodeURIComponent(letter)}</text></svg>`;

  const containerStyle: React.CSSProperties = {
    width: size, height: size, flexShrink: 0,
  };
  if (style) Object.assign(containerStyle, style);

  return src ? (
    <img
      src={src}
      alt={name}
      className={className}
      style={{ ...containerStyle, borderRadius: radius, objectFit: 'cover' }}
      loading="lazy"
      onError={() => setSrc(null)}
    />
  ) : (
    <img
      src={letterSvg}
      alt={name}
      className={className}
      style={{ ...containerStyle, borderRadius: radius }}
    />
  );
}

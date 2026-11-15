// Crop a Minecraft skin PNG (data URL) into a square player head for
// avatars. Skin layout: the front face is the 8x8 region at (8, 8); modern
// 64x64 skins add a hat overlay at (40, 8) drawn on top with its own alpha.
// Legacy 64x32 skins have no overlay region. Nearest-neighbour upscale
// keeps the pixel look. Results are cached per uuid AND skin content: when
// the backend returns a changed skin, the old cropped head must not be
// reused for the same account.

const HEAD_OUT = 128;

const headCache = new Map<string, Promise<string | null>>();

/** Deterministic, dependency-free FNV-1a hash for the skin data URL. */
function skinHash(value: string): number {
  let hash = 0x811c9dc5;
  for (let i = 0; i < value.length; i++) {
    hash ^= value.charCodeAt(i);
    hash = Math.imul(hash, 0x01000193);
  }
  return hash >>> 0;
}

/** Cache key for a crop: identity plus the exact skin payload. */
export function skinHeadCacheKey(uuid: string, skinDataUrl: string): string {
  const id = uuid.replace(/-/g, '').toLowerCase();
  return `${id}:${skinDataUrl.length}:${skinHash(skinDataUrl).toString(16)}`;
}

function loadImage(src: string): Promise<HTMLImageElement> {
  return new Promise((resolve, reject) => {
    const img = new Image();
    img.onload = () => resolve(img);
    img.onerror = () => reject(new Error('skin image failed to decode'));
    img.src = src;
  });
}

async function cropHead(skinDataUrl: string): Promise<string | null> {
  const img = await loadImage(skinDataUrl);
  if (img.width !== 64 || (img.height !== 64 && img.height !== 32)) {
    return null;
  }
  const canvas = document.createElement('canvas');
  canvas.width = HEAD_OUT;
  canvas.height = HEAD_OUT;
  const ctx = canvas.getContext('2d');
  if (!ctx) return null;
  ctx.imageSmoothingEnabled = false;
  // Front face.
  ctx.drawImage(img, 8, 8, 8, 8, 0, 0, HEAD_OUT, HEAD_OUT);
  // Hat overlay (modern skins only) — drawn over the face; fully transparent
  // pixels leave the face intact.
  if (img.height === 64) {
    ctx.drawImage(img, 40, 8, 8, 8, 0, 0, HEAD_OUT, HEAD_OUT);
  }
  return canvas.toDataURL('image/png');
}

/**
 * Head image (data URL) for an account, cropped from its skin. `uuid` is the
 * cache key (normalized the same way the backend does: dashes stripped,
 * lowercase). Returns null when the skin cannot be decoded/cropped.
 */
export function headFromSkin(uuid: string, skinDataUrl: string): Promise<string | null> {
  const key = skinHeadCacheKey(uuid, skinDataUrl);
  let cached = headCache.get(key);
  if (!cached) {
    cached = cropHead(skinDataUrl).catch(() => null);
    headCache.set(key, cached);
  }
  return cached;
}

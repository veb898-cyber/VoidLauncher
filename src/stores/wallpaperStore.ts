import { create } from 'zustand';
import { isWallpaperId, WALLPAPER_IDS, type WallpaperId } from '../lib/wallpapers';

const STORAGE_KEY = 'voidlauncher-lg-wallpaper';

function getInitialWallpaper(): WallpaperId {
  try {
    const stored = localStorage.getItem(STORAGE_KEY);
    if (isWallpaperId(stored)) return stored;
  } catch {}
  // Fresh default: the liquid ribbons read best of the four (owner wave 26:
  // «по дефолту должны стоять жидкие ленты»). An explicit choice — including
  // "none" — is persisted above and always wins.
  return 'ribbons';
}

interface WallpaperState {
  wallpaper: WallpaperId;
  setWallpaper: (wallpaper: WallpaperId) => void;
}

/**
 * Backdrop choice for the Liquid Glass appearance only.
 *
 * Separate from `themeStore` because it is not a theme: it has no effect unless
 * Liquid Glass is on, and its values would be meaningless in the other four
 * appearances. Persisted immediately rather than through the config save, for
 * the same reason the theme is — it is a rendering preference, not app state.
 */
export const useWallpaperStore = create<WallpaperState>((set) => ({
  wallpaper: getInitialWallpaper(),
  setWallpaper: (wallpaper: WallpaperId) => {
    if (!WALLPAPER_IDS.includes(wallpaper)) return;
    set({ wallpaper });
    try {
      localStorage.setItem(STORAGE_KEY, wallpaper);
    } catch {}
  },
}));

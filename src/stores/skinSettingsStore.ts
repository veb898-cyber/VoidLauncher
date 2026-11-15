import { create } from 'zustand';

/** How cached player skins are refreshed. */
export type SkinRefreshMode = 'auto' | 'manual';

export const SKIN_REFRESH_STORAGE_KEY = 'voidlauncher-skin-refresh-mode';
export const DEFAULT_SKIN_REFRESH_MODE: SkinRefreshMode = 'auto';

export function loadSkinRefreshMode(): SkinRefreshMode {
  try {
    const raw = localStorage.getItem(SKIN_REFRESH_STORAGE_KEY);
    return raw === 'manual' ? 'manual' : 'auto';
  } catch {
    return DEFAULT_SKIN_REFRESH_MODE;
  }
}

function persistSkinRefreshMode(mode: SkinRefreshMode): void {
  try {
    localStorage.setItem(SKIN_REFRESH_STORAGE_KEY, mode);
  } catch {
    // Storage unavailable: the in-memory mode still applies for this session.
  }
}

interface SkinSettingsState {
  refreshMode: SkinRefreshMode;
  setRefreshMode: (mode: SkinRefreshMode) => void;
}

export const useSkinSettingsStore = create<SkinSettingsState>((set, get) => ({
  refreshMode: loadSkinRefreshMode(),

  setRefreshMode: (mode) => {
    if (get().refreshMode === mode) return;
    set({ refreshMode: mode });
    persistSkinRefreshMode(mode);
  },
}));

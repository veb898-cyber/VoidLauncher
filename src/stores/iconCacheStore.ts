import { create } from 'zustand';

// Session-only icon cache (renderer memory). Nothing is persisted to disk:
// local icons are re-extracted from their archives after a restart, and
// network icons are re-fetched — same model as Prism Launcher.
interface IconCacheState {
  cache: Map<string, string>;
  getIcon: (key: string) => string | undefined;
  setIcon: (key: string, value: string) => void;
}

export const useIconCacheStore = create<IconCacheState>((set, get) => ({
  cache: new Map(),

  getIcon: (key) => get().cache.get(key),

  setIcon: (key, value) => {
    const newCache = new Map(get().cache);
    newCache.set(key, value);
    set({ cache: newCache });
  },
}));

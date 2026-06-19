import { create } from 'zustand';

interface IconCacheState {
  cache: Map<string, string>;
  hydrated: boolean;
  setCache: (entries: Record<string, string>) => void;
  getIcon: (key: string) => string | undefined;
  setIcon: (key: string, value: string) => void;
  markHydrated: () => boolean;
}

export const useIconCacheStore = create<IconCacheState>((set, get) => ({
  cache: new Map(),
  hydrated: false,

  setCache: (entries) => {
    const map = new Map(Object.entries(entries));
    set({ cache: map, hydrated: true });
  },

  getIcon: (key) => get().cache.get(key),

  setIcon: (key, value) => {
    const newCache = new Map(get().cache);
    newCache.set(key, value);
    set({ cache: newCache });
  },

  markHydrated: () => {
    const wasHydrated = get().hydrated;
    if (!wasHydrated) set({ hydrated: true });
    return !wasHydrated;
  },
}));
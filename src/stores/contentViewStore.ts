import { create } from 'zustand';

const STORAGE_KEY = 'voidlauncher-content-cards';

function getInitialCards(): boolean {
  try {
    const stored = localStorage.getItem(STORAGE_KEY);
    if (stored === '0') return false;
    if (stored === '1') return true;
  } catch {}
  // Default on (owner: «добавь опцию которая будет по дефолту включена») —
  // a fresh install gets the card lists.
  return true;
}

interface ContentViewState {
  /** Card interface for the mods / resource packs / shaders sections
   *  (owner: «использовать старый/новый интерфейс», new = one card per
   *  entry). Only the Liquid Glass rendering has the card look, so the
   *  flag is inert in the normal themes. */
  cards: boolean;
  setCards: (cards: boolean) => void;
}

/**
 * Rendering preference, persisted immediately like the theme/wallpaper —
 * it is a view choice, not app state, so it does not go through the
 * config save.
 */
export const useContentViewStore = create<ContentViewState>((set) => ({
  cards: getInitialCards(),
  setCards: (cards: boolean) => {
    set({ cards });
    try {
      localStorage.setItem(STORAGE_KEY, cards ? '1' : '0');
    } catch {}
  },
}));

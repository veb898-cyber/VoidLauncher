import { create } from 'zustand';
import { persist } from 'zustand/middleware';
import { type Language } from '../lib/i18n';

interface LanguageState {
  language: Language;
  setLanguage: (lang: Language) => void;
}

/**
 * Persists the active UI language to localStorage.
 * Both `useT()` (hook) and `t()` (bare) read directly from this store.
 */
export const useLanguageStore = create<LanguageState>()(
  persist(
    (set) => ({
      language: 'en',
      setLanguage: (lang) => {
        set({ language: lang });
      },
    }),
    {
      name: 'voidlauncher.language',
    }
  )
);

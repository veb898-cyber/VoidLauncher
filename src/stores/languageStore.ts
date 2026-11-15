import { create } from 'zustand';
import { persist } from 'zustand/middleware';
import { type Language } from '../lib/i18n';

interface LanguageState {
  language: Language;
  setLanguage: (lang: Language) => void;
}

function applyHtmlLang(lang: Language) {
  if (typeof document !== 'undefined') {
    // Screen readers and the browser's spellchecker/translation heuristics
    // rely on the document language, not on the React i18n store.
    document.documentElement.lang = lang;
  }
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
        applyHtmlLang(lang);
      },
    }),
    {
      name: 'voidlauncher.language',
      onRehydrateStorage: () => (state) => {
        if (state) applyHtmlLang(state.language);
      },
    }
  )
);

// Persisted language is rehydrated synchronously for localStorage, but keep
// the document in sync even if a future storage implementation becomes async.
applyHtmlLang(useLanguageStore.getState().language);
useLanguageStore.subscribe((state) => applyHtmlLang(state.language));

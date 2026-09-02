import { create } from 'zustand';

export type Theme = 'standard' | 'dark' | 'blueprint' | 'ember';

const STORAGE_KEY = 'voidlauncher-theme';

function getInitialTheme(): Theme {
  try {
    const stored = localStorage.getItem(STORAGE_KEY);
    if (stored === 'standard' || stored === 'dark' || stored === 'blueprint' || stored === 'ember') {
      return stored;
    }
  } catch {}
  return 'standard';
}

function applyTheme(theme: Theme) {
  document.documentElement.setAttribute('data-theme', theme);
  // color-scheme: dark for all current themes (OLED Light was removed)
  document.documentElement.style.colorScheme = 'dark';
}

interface ThemeState {
  theme: Theme;
  setTheme: (theme: Theme) => void;
}

export const useThemeStore = create<ThemeState>((set) => {
  const initial = getInitialTheme();
  applyTheme(initial);

  return {
    theme: initial,
    setTheme: (theme: Theme) => {
      set({ theme });
      applyTheme(theme);
      try {
        localStorage.setItem(STORAGE_KEY, theme);
      } catch {}
    },
  };
});

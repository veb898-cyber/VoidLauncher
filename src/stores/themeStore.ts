import { create } from 'zustand';

export type Theme = 'standard' | 'dark' | 'light';

const STORAGE_KEY = 'voidlauncher-theme';

function getInitialTheme(): Theme {
  try {
    const stored = localStorage.getItem(STORAGE_KEY);
    if (stored === 'standard' || stored === 'dark' || stored === 'light') {
      return stored;
    }
  } catch {}
  return 'standard';
}

function applyTheme(theme: Theme) {
  document.documentElement.setAttribute('data-theme', theme);
  // Set color-scheme for native <select> popup
  document.documentElement.style.colorScheme = theme === 'light' ? 'light' : 'dark';
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

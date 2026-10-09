import { create } from 'zustand';

export type Theme = 'standard' | 'dark' | 'blueprint' | 'ember';

const THEME_STORAGE_KEY = 'voidlauncher-theme';
const LG_STORAGE_KEY = 'voidlauncher-liquid-glass';
const LG_REALISTIC_KEY = 'voidlauncher-lg-realistic';

/**
 * Value written to `<html data-theme>` while Liquid Glass is active.
 *
 * Deliberately NOT a member of `Theme`. The applied appearance is *derived*
 * from (theme, liquidGlass) by `applyAppearance` and never stored, so
 * "Liquid Glass is on and a normal theme is applied at the same time" is not
 * a representable state — there is no code path that can produce it.
 *
 * Because the Liquid Glass CSS block in index.css is scoped to exactly this
 * value, no `[data-theme="dark"|"blueprint"|"ember"]` rule matches while it is
 * applied: a theme palette cannot leak into Liquid Glass, and Liquid Glass
 * cannot be blended into a theme palette.
 */
const LG_APPEARANCE = 'lg';

function getInitialTheme(): Theme {
  try {
    const stored = localStorage.getItem(THEME_STORAGE_KEY);
    if (stored === 'standard' || stored === 'dark' || stored === 'blueprint' || stored === 'ember') {
      return stored;
    }
  } catch {}
  return 'standard';
}

function getInitialLiquidGlass(): boolean {
  try {
    return localStorage.getItem(LG_STORAGE_KEY) === '1';
  } catch {}
  return false;
}

function getInitialRealisticGlass(): boolean {
  try {
    return localStorage.getItem(LG_REALISTIC_KEY) === '1';
  } catch {}
  return false;
}

/**
 * Single writer of the appearance attributes. Keeping this the only place
 * that touches `data-theme` is what makes the Liquid Glass / normal theme
 * separation structural rather than a convention.
 *
 * `data-lg-realistic` is a modifier of the Liquid Glass material only: the
 * CSS that reads it is scoped to `[data-theme='lg']`, so the flag is inert
 * while Liquid Glass is off (same pattern as the wallpaper choice).
 */
function applyAppearance(theme: Theme, liquidGlass: boolean, realisticGlass: boolean) {
  document.documentElement.setAttribute('data-theme', liquidGlass ? LG_APPEARANCE : theme);
  document.documentElement.setAttribute('data-lg-realistic', realisticGlass ? '1' : '0');
  // Every current appearance, Liquid Glass included, is dark
  // (OLED Light was removed).
  document.documentElement.style.colorScheme = 'dark';
}

interface ThemeState {
  /** The normal theme. Always a valid `Theme`, never cleared and never
   *  overwritten by the Liquid Glass flag — it is what gets restored when
   *  Liquid Glass is switched off. */
  theme: Theme;
  /** Experimental Liquid Glass appearance. A separate mode, not a theme. */
  liquidGlass: boolean;
  /**
   * Realistic (iOS-style) variant of the Liquid Glass material: a much
   * clearer fill so content and the backdrop read through the panels.
   * Off by default; only rendered while `liquidGlass` is on.
   */
  realisticGlass: boolean;
  setTheme: (theme: Theme) => void;
  setLiquidGlass: (enabled: boolean) => void;
  setRealisticGlass: (enabled: boolean) => void;
}

export const useThemeStore = create<ThemeState>((set, get) => {
  const initialTheme = getInitialTheme();
  const initialLiquidGlass = getInitialLiquidGlass();
  const initialRealisticGlass = getInitialRealisticGlass();
  applyAppearance(initialTheme, initialLiquidGlass, initialRealisticGlass);

  return {
    theme: initialTheme,
    liquidGlass: initialLiquidGlass,
    realisticGlass: initialRealisticGlass,

    setTheme: (theme: Theme) => {
      // Locked while Liquid Glass is on: the normal theme cannot change
      // behind its back, so switching Liquid Glass off always restores the
      // theme that was actually selected. The settings UI disables the
      // control; this is the guard that holds even if something else calls it.
      if (get().liquidGlass) return;
      set({ theme });
      applyAppearance(theme, false, get().realisticGlass);
      try {
        localStorage.setItem(THEME_STORAGE_KEY, theme);
      } catch {}
    },

    setLiquidGlass: (enabled: boolean) => {
      // The stored normal theme is intentionally left untouched, so turning
      // Liquid Glass off brings back exactly what was selected before.
      set({ liquidGlass: enabled });
      applyAppearance(get().theme, enabled, get().realisticGlass);
      try {
        localStorage.setItem(LG_STORAGE_KEY, enabled ? '1' : '0');
      } catch {}
    },

    setRealisticGlass: (enabled: boolean) => {
      // Kept even while Liquid Glass is off (the UI locks the control then),
      // exactly like the wallpaper choice: the setting comes back with the
      // mode instead of being silently reset behind the user's back.
      set({ realisticGlass: enabled });
      applyAppearance(get().theme, get().liquidGlass, enabled);
      try {
        localStorage.setItem(LG_REALISTIC_KEY, enabled ? '1' : '0');
      } catch {}
    },
  };
});

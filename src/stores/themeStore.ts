import { create } from 'zustand';

export type Theme = 'standard' | 'dark' | 'blueprint' | 'ember';

const THEME_STORAGE_KEY = 'voidlauncher-theme';
const LG_STORAGE_KEY = 'voidlauncher-liquid-glass';
const MOTION_STORAGE_KEY = 'voidlauncher-liquid-motion';
const GOOEY_STORAGE_KEY = 'voidlauncher-gooey';

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

// Both motion options default to ON: absence of a stored value means the
// fresh install (and every pre-existing user) gets the liquid feel.
function getInitialLiquidMotion(): boolean {
  try {
    return localStorage.getItem(MOTION_STORAGE_KEY) !== '0';
  } catch {}
  return true;
}

function getInitialGooey(): boolean {
  try {
    return localStorage.getItem(GOOEY_STORAGE_KEY) !== '0';
  } catch {}
  return true;
}

/**
 * Single writer of the appearance attributes. Keeping this the only place
 * that touches `data-theme` is what makes the Liquid Glass / normal theme
 * separation structural rather than a convention.
 *
 * `data-lg-realistic` is a modifier of the Liquid Glass material only: the
 * CSS that reads it is scoped to `[data-theme='lg']`, so the flag is inert
 * while Liquid Glass is off (same pattern as the wallpaper choice). The
 * realistic (iOS-style) material is the only Liquid Glass look now — the
 * darker experimental panels were dropped on the owner's order, so the
 * flag is always on while Liquid Glass is on.
 */
function applyAppearance(theme: Theme, liquidGlass: boolean) {
  document.documentElement.setAttribute('data-theme', liquidGlass ? LG_APPEARANCE : theme);
  document.documentElement.setAttribute('data-lg-realistic', '1');
  // Every current appearance, Liquid Glass included, is dark
  // (OLED Light was removed).
  document.documentElement.style.colorScheme = 'dark';
}

/**
 * Independent of the theme derivation: the motion flags only gate CSS blocks
 * (`[data-motion='liquid']`) and React branches (gooey wrappers), so they get
 * their own tiny writer instead of threading through applyAppearance.
 */
function applyMotionFlags(liquidMotion: boolean, gooey: boolean) {
  if (liquidMotion) {
    document.documentElement.setAttribute('data-motion', 'liquid');
  } else {
    document.documentElement.removeAttribute('data-motion');
  }
  if (gooey) {
    document.documentElement.setAttribute('data-gooey', 'on');
  } else {
    document.documentElement.removeAttribute('data-gooey');
  }
}

interface ThemeState {
  /** The normal theme. Always a valid `Theme`, never cleared and never
   *  overwritten by the Liquid Glass flag — it is what gets restored when
   *  Liquid Glass is switched off. */
  theme: Theme;
  /** Experimental Liquid Glass appearance. A separate mode, not a theme. */
  liquidGlass: boolean;
  /** Springy transition curves + elastic press (CSS `[data-motion='liquid']`). */
  liquidMotion: boolean;
  /** Liquid trailing on sliding indicators (React gooey wrappers). */
  gooey: boolean;
  setTheme: (theme: Theme) => void;
  setLiquidGlass: (enabled: boolean) => void;
  setLiquidMotion: (enabled: boolean) => void;
  setGooey: (enabled: boolean) => void;
}

export const useThemeStore = create<ThemeState>((set, get) => {
  const initialTheme = getInitialTheme();
  const initialLiquidGlass = getInitialLiquidGlass();
  const initialLiquidMotion = getInitialLiquidMotion();
  const initialGooey = getInitialGooey();
  applyAppearance(initialTheme, initialLiquidGlass);
  applyMotionFlags(initialLiquidMotion, initialGooey);

  return {
    theme: initialTheme,
    liquidGlass: initialLiquidGlass,
    liquidMotion: initialLiquidMotion,
    gooey: initialGooey,

    setTheme: (theme: Theme) => {
      // Locked while Liquid Glass is on: the normal theme cannot change
      // behind its back, so switching Liquid Glass off always restores the
      // theme that was actually selected. The settings UI disables the
      // control; this is the guard that holds even if something else calls it.
      if (get().liquidGlass) return;
      set({ theme });
      applyAppearance(theme, false);
      try {
        localStorage.setItem(THEME_STORAGE_KEY, theme);
      } catch {}
    },

    setLiquidGlass: (enabled: boolean) => {
      // The stored normal theme is intentionally left untouched, so turning
      // Liquid Glass off brings back exactly what was selected before.
      set({ liquidGlass: enabled });
      applyAppearance(get().theme, enabled);
      try {
        localStorage.setItem(LG_STORAGE_KEY, enabled ? '1' : '0');
      } catch {}
    },

    setLiquidMotion: (enabled: boolean) => {
      set({ liquidMotion: enabled });
      applyMotionFlags(enabled, get().gooey);
      try {
        localStorage.setItem(MOTION_STORAGE_KEY, enabled ? '1' : '0');
      } catch {}
    },

    setGooey: (enabled: boolean) => {
      set({ gooey: enabled });
      applyMotionFlags(get().liquidMotion, enabled);
      try {
        localStorage.setItem(GOOEY_STORAGE_KEY, enabled ? '1' : '0');
      } catch {}
    },
  };
});

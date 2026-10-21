import { create } from 'zustand';

export type Theme =
  | 'standard'
  | 'ember'
  | 'dark'
  | 'phosphor'
  | 'carbon'
  | 'blueprint';

const THEMES: readonly Theme[] = [
  'standard',
  'ember',
  'dark',
  'phosphor',
  'carbon',
  'blueprint',
];

const THEME_STORAGE_KEY = 'voidlauncher-theme';
const LG_STORAGE_KEY = 'voidlauncher-liquid-glass';

function isTheme(value: string | null): value is Theme {
  return value !== null && (THEMES as readonly string[]).includes(value);
}

/**
 * Value written to `<html data-theme>` while Liquid Glass is active.
 *
 * Deliberately NOT a member of `Theme`. The applied appearance is *derived*
 * from (theme, liquidGlass) by `applyAppearance` and never stored, so
 * "Liquid Glass is on and a normal theme is applied at the same time" is not
 * a representable state — there is no code path that can produce it.
 *
 * Because the Liquid Glass CSS block in index.css is scoped to exactly this
 * value, no normal-theme rule matches while it is applied: a theme palette
 * cannot leak into Liquid Glass, and Liquid Glass cannot be blended into a
 * theme palette.
 */
const LG_APPEARANCE = 'lg';

function getInitialTheme(): Theme {
  try {
    const stored = localStorage.getItem(THEME_STORAGE_KEY);
    if (isTheme(stored)) {
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
  // Every normal theme and Liquid Glass are dark appearances, so the native
  // controls always use the dark colour scheme.
  document.documentElement.style.colorScheme = 'dark';
}

/**
 * Liquid motion is not a setting any more (owner: always on): the spring
 * curves are gated by the `[data-motion='liquid']` CSS blocks, and the gooey
 * trails are React branches behind `useGooey`. Reduced-motion stays honoured —
 * the CSS media blocks neutralise the curves, and `useGooey` switches the
 * wrappers off. The inline bootstrap in index.html sets the same attribute
 * before first paint; this write keeps the attribute correct even if that
 * script was skipped.
 */
document.documentElement.setAttribute('data-motion', 'liquid');

interface ThemeState {
  /** The normal theme. Always a valid `Theme`, never cleared and never
   *  overwritten by the Liquid Glass flag — it is what gets restored when
   *  Liquid Glass is switched off. */
  theme: Theme;
  /** Experimental Liquid Glass appearance. A separate mode, not a theme. */
  liquidGlass: boolean;
  setTheme: (theme: Theme) => void;
  setLiquidGlass: (enabled: boolean) => void;
}

export const useThemeStore = create<ThemeState>((set, get) => {
  const initialTheme = getInitialTheme();
  const initialLiquidGlass = getInitialLiquidGlass();
  applyAppearance(initialTheme, initialLiquidGlass);

  return {
    theme: initialTheme,
    liquidGlass: initialLiquidGlass,

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
  };
});

import { create } from 'zustand';

export type Font = 'default' | 'monocraft';
/** Font used by the console and game logs.
 *  - `auto`     — use the selected interface font (default behaviour)
 *  - `default`  — the launcher's standard monospaced stack
 *  - `monocraft` — pixel Minecraft-style font */
export type ConsoleFont = 'auto' | 'default' | 'monocraft';

const STORAGE_KEY = 'voidlauncher-font';
const CONSOLE_KEY = 'voidlauncher-console-font';
const LEGACY_SCOPE_KEY = 'voidlauncher-font-scope';

function getInitialFont(): Font {
  try {
    const stored = localStorage.getItem(STORAGE_KEY);
    // Legacy "inter" values are treated as default.
    if (stored === 'default' || stored === 'monocraft') {
      return stored;
    }
  } catch {}
  return 'default';
}

function getInitialConsoleFont(): ConsoleFont {
  try {
    const stored = localStorage.getItem(CONSOLE_KEY);
    if (stored === 'auto' || stored === 'default' || stored === 'monocraft') {
      return stored;
    }
    // Migrate from the short-lived "font scope" setting (0.1.9 dev builds):
    // "ui" meant a default console, "all"/"logs" meant following the UI font.
    const scope = localStorage.getItem(LEGACY_SCOPE_KEY);
    if (scope === 'ui') return 'default';
    if (scope === 'all' || scope === 'logs') return 'auto';
  } catch {}
  return 'auto';
}

function setAttr(name: string, value: string | null) {
  if (!value || value === 'default') {
    document.documentElement.removeAttribute(name);
  } else {
    document.documentElement.setAttribute(name, value);
  }
}

/**
 * Remaps both font tokens:
 *   - `data-font`      → `--font-ui`  (interface)
 *   - `data-font-mono` → `--font-mono` (console, game logs)
 * The console font follows the interface font when set to `auto`.
 */
function applyFont(font: Font, consoleFont: ConsoleFont) {
  setAttr('data-font', font);
  const mono = consoleFont === 'default' ? 'default' : consoleFont === 'monocraft' ? 'monocraft' : font;
  setAttr('data-font-mono', mono);
}

interface FontState {
  font: Font;
  consoleFont: ConsoleFont;
  setFont: (font: Font) => void;
  setConsoleFont: (consoleFont: ConsoleFont) => void;
}

export const useFontStore = create<FontState>((set, get) => {
  const initialFont = getInitialFont();
  const initialConsoleFont = getInitialConsoleFont();
  applyFont(initialFont, initialConsoleFont);

  return {
    font: initialFont,
    consoleFont: initialConsoleFont,
    setFont: (font: Font) => {
      set({ font });
      applyFont(font, get().consoleFont);
      try {
        localStorage.setItem(STORAGE_KEY, font);
      } catch {}
    },
    setConsoleFont: (consoleFont: ConsoleFont) => {
      set({ consoleFont });
      applyFont(get().font, consoleFont);
      try {
        localStorage.setItem(CONSOLE_KEY, consoleFont);
      } catch {}
    },
  };
});
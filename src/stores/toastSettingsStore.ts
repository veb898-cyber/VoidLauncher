import { create } from 'zustand';

/** Toast categories the user can switch off independently. */
export type ToastCategory = 'success' | 'warning' | 'error';

export interface ToastPreferences {
  success: boolean;
  warning: boolean;
  error: boolean;
}

export const TOAST_SETTINGS_STORAGE_KEY = 'voidlauncher-toast-settings';

/** Success and warning toasts are opt-in; errors stay visible by default. */
export const DEFAULT_TOAST_PREFERENCES: ToastPreferences = {
  success: false,
  warning: false,
  error: true,
};

function coercePreferences(value: unknown): ToastPreferences {
  if (!value || typeof value !== 'object') return DEFAULT_TOAST_PREFERENCES;
  const record = value as Partial<Record<keyof ToastPreferences, unknown>>;
  // Only an explicit `true` opts a silent-by-default category in. Errors are
  // the opposite: an explicit `false` is required to hide them, so an old or
  // partial payload never silently hides failures.
  return {
    success: record.success === true,
    warning: record.warning === true,
    error: record.error !== false,
  };
}

/** Read the persisted preferences (defaults on first launch or bad data). */
export function loadToastPreferences(): ToastPreferences {
  try {
    const raw = localStorage.getItem(TOAST_SETTINGS_STORAGE_KEY);
    if (!raw) return DEFAULT_TOAST_PREFERENCES;
    return coercePreferences(JSON.parse(raw));
  } catch {
    return DEFAULT_TOAST_PREFERENCES;
  }
}

/** Whether a toast type is allowed by the given preferences. */
export function isToastCategoryEnabled(
  type: ToastCategory | 'info',
  preferences: ToastPreferences,
): boolean {
  return type === 'info' || preferences[type];
}

function persistPreferences(preferences: ToastPreferences) {
  try {
    localStorage.setItem(TOAST_SETTINGS_STORAGE_KEY, JSON.stringify(preferences));
  } catch {
    // Storage may be unavailable (private mode/quota); the in-memory setting
    // still applies for this session.
  }
}

interface ToastSettingsState extends ToastPreferences {
  setCategory: (category: ToastCategory, enabled: boolean) => void;
}

export const useToastSettingsStore = create<ToastSettingsState>((set, get) => ({
  ...loadToastPreferences(),

  setCategory: (category, enabled) => {
    if (get()[category] === enabled) return;
    set((state) => ({ ...state, [category]: enabled }));
    const { success, warning, error } = get();
    persistPreferences({ success, warning, error });
  },
}));

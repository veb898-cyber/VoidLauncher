import { create } from 'zustand';

/** Toast categories the user can switch off independently. */
export type ToastCategory = 'success' | 'warning' | 'error';

export interface ToastPreferences {
  success: boolean;
  warning: boolean;
  error: boolean;
}

export const TOAST_SETTINGS_STORAGE_KEY = 'voidlauncher-toast-settings';

/** Visible by default: keeps the pre-setting behaviour unchanged. */
export const DEFAULT_TOAST_PREFERENCES: ToastPreferences = {
  success: true,
  warning: true,
  error: true,
};

function coercePreferences(value: unknown): ToastPreferences {
  if (!value || typeof value !== 'object') return DEFAULT_TOAST_PREFERENCES;
  const record = value as Partial<Record<keyof ToastPreferences, unknown>>;
  // Missing or malformed fields stay enabled: an old/partial payload must
  // never silently hide notifications the user did not switch off.
  return {
    success: record.success !== false,
    warning: record.warning !== false,
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

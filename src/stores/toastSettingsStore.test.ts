import { beforeEach, describe, expect, it } from 'vitest';
import {
  DEFAULT_TOAST_PREFERENCES,
  TOAST_SETTINGS_STORAGE_KEY,
  isToastCategoryEnabled,
  loadToastPreferences,
  useToastSettingsStore,
} from './toastSettingsStore';

beforeEach(() => {
  localStorage.clear();
  useToastSettingsStore.setState(DEFAULT_TOAST_PREFERENCES);
});

describe('toast settings', () => {
  it('keeps every category enabled by default', () => {
    expect(loadToastPreferences()).toEqual({
      success: true,
      warning: true,
      error: true,
    });
  });

  it('restores a persisted opt-out after a restart', () => {
    useToastSettingsStore.getState().setCategory('success', false);

    const stored = JSON.parse(localStorage.getItem(TOAST_SETTINGS_STORAGE_KEY) ?? '{}');
    expect(stored).toEqual({ success: false, warning: true, error: true });
    expect(loadToastPreferences()).toEqual(stored);
  });

  it('switches categories independently', () => {
    const store = useToastSettingsStore.getState();
    store.setCategory('warning', false);
    expect(useToastSettingsStore.getState().warning).toBe(false);
    expect(useToastSettingsStore.getState().success).toBe(true);
    expect(useToastSettingsStore.getState().error).toBe(true);

    store.setCategory('error', false);
    expect(useToastSettingsStore.getState().error).toBe(false);
    expect(useToastSettingsStore.getState().success).toBe(true);

    store.setCategory('success', false);
    expect(useToastSettingsStore.getState()).toMatchObject({
      success: false,
      warning: false,
      error: false,
    });
  });

  it('treats missing or malformed stored fields as enabled', () => {
    localStorage.setItem(TOAST_SETTINGS_STORAGE_KEY, '{"success":false}');
    expect(loadToastPreferences()).toEqual({
      success: false,
      warning: true,
      error: true,
    });

    localStorage.setItem(TOAST_SETTINGS_STORAGE_KEY, '{not json');
    expect(loadToastPreferences()).toEqual(DEFAULT_TOAST_PREFERENCES);
  });

  it('never filters informational toasts', () => {
    const allOff = { success: false, warning: false, error: false };
    expect(isToastCategoryEnabled('success', allOff)).toBe(false);
    expect(isToastCategoryEnabled('warning', allOff)).toBe(false);
    expect(isToastCategoryEnabled('error', allOff)).toBe(false);
    expect(isToastCategoryEnabled('info', allOff)).toBe(true);
  });
});

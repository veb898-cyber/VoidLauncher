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
  it('shows only error toasts by default', () => {
    expect(loadToastPreferences()).toEqual({
      success: false,
      warning: false,
      error: true,
    });
  });

  it('preserves an explicit opt-in after a restart', () => {
    useToastSettingsStore.getState().setCategory('success', true);

    const stored = JSON.parse(localStorage.getItem(TOAST_SETTINGS_STORAGE_KEY) ?? '{}');
    expect(stored).toEqual({ success: true, warning: false, error: true });
    expect(loadToastPreferences()).toEqual(stored);
  });

  it('preserves an explicit opt-out of error toasts', () => {
    useToastSettingsStore.getState().setCategory('error', false);
    expect(loadToastPreferences()).toEqual({
      success: false,
      warning: false,
      error: false,
    });
  });

  it('switches categories independently', () => {
    const store = useToastSettingsStore.getState();
    store.setCategory('warning', true);
    expect(useToastSettingsStore.getState().warning).toBe(true);
    expect(useToastSettingsStore.getState().success).toBe(false);
    expect(useToastSettingsStore.getState().error).toBe(true);

    store.setCategory('success', true);
    expect(useToastSettingsStore.getState()).toMatchObject({
      success: true,
      warning: true,
      error: true,
    });

    store.setCategory('warning', false);
    expect(useToastSettingsStore.getState()).toMatchObject({
      success: true,
      warning: false,
      error: true,
    });
  });

  it('uses the new defaults for missing fields and malformed payloads', () => {
    localStorage.setItem(TOAST_SETTINGS_STORAGE_KEY, '{"success":true}');
    expect(loadToastPreferences()).toEqual({
      success: true,
      warning: false,
      error: true,
    });

    localStorage.setItem(TOAST_SETTINGS_STORAGE_KEY, '{"error":false}');
    expect(loadToastPreferences()).toEqual({
      success: false,
      warning: false,
      error: false,
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

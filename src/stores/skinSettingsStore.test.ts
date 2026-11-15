import { beforeEach, describe, expect, it } from 'vitest';
import {
  DEFAULT_SKIN_REFRESH_MODE,
  SKIN_REFRESH_STORAGE_KEY,
  loadSkinRefreshMode,
  useSkinSettingsStore,
} from './skinSettingsStore';

beforeEach(() => {
  localStorage.clear();
  useSkinSettingsStore.setState({ refreshMode: DEFAULT_SKIN_REFRESH_MODE });
});

describe('skin refresh preference', () => {
  it('defaults to automatic refresh', () => {
    expect(loadSkinRefreshMode()).toBe('auto');
  });

  it('persists manual mode across a restart', () => {
    useSkinSettingsStore.getState().setRefreshMode('manual');
    expect(localStorage.getItem(SKIN_REFRESH_STORAGE_KEY)).toBe('manual');
    expect(loadSkinRefreshMode()).toBe('manual');
  });

  it('falls back to automatic for malformed values', () => {
    localStorage.setItem(SKIN_REFRESH_STORAGE_KEY, 'sometimes');
    expect(loadSkinRefreshMode()).toBe('auto');
  });
});

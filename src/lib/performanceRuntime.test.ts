import { beforeEach, describe, expect, it } from 'vitest';
import {
  LEGACY_EFFECTS_STORAGE_KEY,
  LEGACY_PROFILE_STORAGE_KEY,
  REDUCE_EFFECTS_STORAGE_KEY,
  inferReduceInterfaceEffects,
  migrateLegacyPerformanceSettings,
} from './performanceRuntime';

beforeEach(() => {
  localStorage.clear();
});

describe('legacy performance migration', () => {
  it('uses the safe default for a fresh install', () => {
    expect(migrateLegacyPerformanceSettings()).toBe(false);
    expect(localStorage.getItem(REDUCE_EFFECTS_STORAGE_KEY)).toBe('0');
    expect(localStorage.getItem(LEGACY_PROFILE_STORAGE_KEY)).toBeNull();
    expect(localStorage.getItem(LEGACY_EFFECTS_STORAGE_KEY)).toBeNull();
  });

  it('keeps the full interface for the old standard profile', () => {
    localStorage.setItem(LEGACY_PROFILE_STORAGE_KEY, 'standard');
    expect(migrateLegacyPerformanceSettings()).toBe(false);
  });

  it('carries the saver intent into the reduce switch', () => {
    localStorage.setItem(LEGACY_PROFILE_STORAGE_KEY, 'saver');
    expect(migrateLegacyPerformanceSettings()).toBe(true);
  });

  it('carries the balanced intent into the reduce switch', () => {
    localStorage.setItem(LEGACY_PROFILE_STORAGE_KEY, 'balanced');
    expect(migrateLegacyPerformanceSettings()).toBe(true);
  });

  it('infers custom state from the old controlled effects only', () => {
    expect(
      inferReduceInterfaceEffects('custom', {
        hoverAnimations: false,
        wallpaperAnimation: false,
        customScrollbar: false,
        skinAnimation: false,
      }),
    ).toBe(true);
    expect(
      inferReduceInterfaceEffects('custom', {
        hoverAnimations: true,
        uiTransitions: true,
        decorativeAnimations: true,
        gooeyEffects: true,
        heavyShadows: true,
        wallpaperAnimation: false,
        customScrollbar: false,
        skinAnimation: false,
      }),
    ).toBe(false);
  });

  it('reads the old effects map when the profile key is missing', () => {
    localStorage.setItem(
      LEGACY_EFFECTS_STORAGE_KEY,
      JSON.stringify({ gooeyEffects: false }),
    );
    expect(migrateLegacyPerformanceSettings()).toBe(true);
    expect(localStorage.getItem(LEGACY_EFFECTS_STORAGE_KEY)).toBeNull();
  });

  it('does not overwrite an existing new value and still cleans old keys', () => {
    localStorage.setItem(REDUCE_EFFECTS_STORAGE_KEY, '0');
    localStorage.setItem(LEGACY_PROFILE_STORAGE_KEY, 'saver');
    localStorage.setItem(LEGACY_EFFECTS_STORAGE_KEY, '{}');
    expect(migrateLegacyPerformanceSettings()).toBe(false);
    expect(localStorage.getItem(REDUCE_EFFECTS_STORAGE_KEY)).toBe('0');
    expect(localStorage.getItem(LEGACY_PROFILE_STORAGE_KEY)).toBeNull();
    expect(localStorage.getItem(LEGACY_EFFECTS_STORAGE_KEY)).toBeNull();
  });

  it('tolerates unreadable or unknown legacy state', () => {
    localStorage.setItem(LEGACY_PROFILE_STORAGE_KEY, 'turbo');
    localStorage.setItem(LEGACY_EFFECTS_STORAGE_KEY, '{not json');
    expect(migrateLegacyPerformanceSettings()).toBe(false);
  });
});

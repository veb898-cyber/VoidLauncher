import { describe, expect, it } from 'vitest';
import { en } from './i18n/en';
import { ru } from './i18n/ru';
import {
  beginCacheClear,
  canDismissCacheClear,
  failCacheClear,
  finishCacheClear,
  initialCacheClearState,
} from './cacheClear';

const idle = { phase: 'idle' as const, error: null, freedBytes: null };

describe('cache clear operation state', () => {
  it('starts from idle and enters the running state', () => {
    const state = beginCacheClear(initialCacheClearState);
    expect(state).toEqual({ phase: 'clearing', error: null, freedBytes: null });
    expect(canDismissCacheClear(state)).toBe(false);
  });

  it('ignores a second start while the first is still running', () => {
    const running = beginCacheClear(initialCacheClearState);
    expect(beginCacheClear(running)).toBe(running);
  });

  it('records an error only for a running operation and allows dismissal again', () => {
    const running = beginCacheClear(initialCacheClearState);
    const failed = failCacheClear(running, 'disk locked');
    expect(failed).toEqual({ phase: 'error', error: 'disk locked', freedBytes: null });
    expect(canDismissCacheClear(failed)).toBe(true);
    expect(failCacheClear(initialCacheClearState, 'late error')).toBe(
      initialCacheClearState,
    );
  });

  it('starts a retry from the error state and clears the old error', () => {
    const failed = failCacheClear(beginCacheClear(initialCacheClearState), 'locked');
    expect(beginCacheClear(failed)).toEqual({
      phase: 'clearing',
      error: null,
      freedBytes: null,
    });
  });

  it('keeps the freed byte count in a success result and allows dismissal', () => {
    const running = beginCacheClear(initialCacheClearState);
    const succeeded = finishCacheClear(running, 52428800);
    expect(succeeded).toEqual({
      phase: 'success',
      error: null,
      freedBytes: 52428800,
    });
    expect(canDismissCacheClear(succeeded)).toBe(true);
    expect(beginCacheClear(succeeded)).toEqual({
      phase: 'clearing',
      error: null,
      freedBytes: null,
    });
  });

  it('finishes only a running operation', () => {
    const running = beginCacheClear(initialCacheClearState);
    expect(finishCacheClear(running, 0)).toEqual({
      phase: 'success',
      error: null,
      freedBytes: 0,
    });
    const failed = failCacheClear(running, 'locked');
    expect(finishCacheClear(failed, 0)).toBe(failed);
    expect(finishCacheClear(idle, 0)).toBe(idle);
  });

  it('keeps every cache-clear string localized', () => {
    const keys = [
      'settings.clear_cache_confirm_title',
      'settings.clear_cache_confirm_desc',
      'settings.clear_cache_delete_label',
      'settings.clear_cache_delete_list',
      'settings.clear_cache_keep_label',
      'settings.clear_cache_keep_list',
      'settings.clear_cache_confirm_note',
      'settings.clear_cache_in_progress',
      'settings.clear_cache_retry_btn',
      'settings.clear_cache_success_title',
      'settings.clear_cache_success',
    ] as const;

    for (const key of keys) {
      expect(en[key]?.trim(), `en ${key}`).toBeTruthy();
      expect(ru[key]?.trim(), `ru ${key}`).toBeTruthy();
    }
  });

  it('does not repeat the success title in the success body', () => {
    expect(en['settings.clear_cache_success'].toLowerCase()).not.toContain(
      en['settings.clear_cache_success_title'].toLowerCase(),
    );
    expect(ru['settings.clear_cache_success'].toLowerCase()).not.toContain(
      ru['settings.clear_cache_success_title'].toLowerCase(),
    );
  });
});

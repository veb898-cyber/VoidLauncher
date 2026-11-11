import { describe, expect, it, vi } from 'vitest';
import {
  installBatchFinished,
  installCompletionAction,
  returnToInstalledContent,
} from './contentInstallFlow';

describe('content install completion', () => {
  it('returns to the installed list after a fully successful single file', () => {
    expect(installCompletionAction(1, 0)).toBe('return');
  });

  it('returns only after the whole multi-file batch succeeded', () => {
    expect(installCompletionAction(3, 0)).toBe('return');
    expect(installBatchFinished(2, 3, true)).toBe(false);
    expect(installBatchFinished(2, 3, false)).toBe(false);
    expect(installBatchFinished(3, 3, false)).toBe(true);
  });

  it('keeps the results screen when at least one file failed', () => {
    expect(installCompletionAction(2, 1)).toBe('results');
    expect(installCompletionAction(0, 1)).toBe('results');
  });

  it('does not claim success for an empty batch', () => {
    expect(installCompletionAction(0, 0)).toBe('results');
  });

  it('is idempotent for a repeated completion event', () => {
    expect(installCompletionAction(1, 0)).toBe('return');
    expect(installCompletionAction(1, 0)).toBe('return');
  });

  it('closes only after the installed-content refresh settles', async () => {
    let release!: () => void;
    const refresh = vi.fn(() => new Promise<void>((resolve) => { release = resolve; }));
    const close = vi.fn();
    const navigation = returnToInstalledContent(refresh, close);

    expect(close).not.toHaveBeenCalled();
    release();
    await navigation;
    expect(refresh).toHaveBeenCalledTimes(1);
    expect(close).toHaveBeenCalledTimes(1);
  });

  it('still returns to the list when the refresh fails', async () => {
    const close = vi.fn();
    await returnToInstalledContent(async () => { throw new Error('read failed'); }, close);
    expect(close).toHaveBeenCalledTimes(1);
  });
});

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { useModpacksStore, type ProgressPayload } from './modpacksStore';

function progress(current: number): ProgressPayload {
  return {
    stage: 'files',
    current,
    total: 100,
    message: `file ${current}`,
  };
}

describe('modpacks progress throttle', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    useModpacksStore.getState().resetInstall();
  });

  afterEach(() => {
    useModpacksStore.getState().resetInstall();
    vi.useRealTimers();
  });

  it('renders the leading event and coalesces a burst into the latest one', () => {
    const store = useModpacksStore.getState();
    store.onImportProgress(progress(1));
    expect(useModpacksStore.getState().progress?.current).toBe(1);

    store.onImportProgress(progress(2));
    store.onImportProgress(progress(3));
    store.onImportProgress(progress(4));
    expect(useModpacksStore.getState().progress?.current).toBe(1);

    vi.advanceTimersByTime(200);
    expect(useModpacksStore.getState().progress?.current).toBe(4);
  });

  it('drops a pending trailing event when the install is reset', () => {
    const store = useModpacksStore.getState();
    store.onImportProgress(progress(1));
    store.onImportProgress(progress(2));

    store.resetInstall();
    vi.advanceTimersByTime(500);

    expect(useModpacksStore.getState().progress).toBeNull();
    expect(useModpacksStore.getState().installing).toBe(false);
  });
});

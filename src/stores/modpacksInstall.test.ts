import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { ImportOutcome } from '../lib/importReport';

const { addToast, loadInstances } = vi.hoisted(() => ({
  addToast: vi.fn(),
  loadInstances: vi.fn(async () => {}),
}));

vi.mock('../components/ui/Toast', () => ({ addToast }));
vi.mock('./instanceStore', () => ({
  useInstanceStore: {
    getState: () => ({ instances: [], loadInstances }),
  },
}));

import { useModpacksStore } from './modpacksStore';

function outcome(warnings: string[]): ImportOutcome {
  return {
    instance: { name: 'Test Pack' },
    report: {
      total_files: 0,
      installed: 0,
      failed: [],
      skipped: 0,
      warnings,
    },
  };
}

describe('modpacks install result reporting', () => {
  beforeEach(() => {
    addToast.mockReset();
    loadInstances.mockClear();
    useModpacksStore.getState().resetInstall();
    useModpacksStore.setState({ installName: 'Test Pack' });
  });

  it('warns instead of claiming success when the prefetch had failures', async () => {
    await useModpacksStore.getState().runInstall(async () => outcome(['loader failed']), 'mr');

    expect(addToast).toHaveBeenCalledWith(
      expect.stringContaining('game files'),
      'warning',
    );
    expect(useModpacksStore.getState().installing).toBe(false);
  });

  it('shows the normal success message when the pack is fully prepared', async () => {
    await useModpacksStore.getState().runInstall(async () => outcome([]), 'mr');

    expect(addToast).toHaveBeenCalledWith(
      expect.stringContaining('installed successfully'),
      'success',
    );
  });
});

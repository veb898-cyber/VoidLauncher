import { beforeEach, describe, expect, it, vi } from 'vitest';
import { t } from '../lib/i18n';
import type { ImportReport } from '../lib/importReport';

const { invoke, addToast, loadInstances } = vi.hoisted(() => ({
  invoke: vi.fn(),
  addToast: vi.fn(),
  loadInstances: vi.fn(async () => {}),
}));

vi.mock('@tauri-apps/api/core', () => ({ invoke }));
vi.mock('../components/ui/Toast', () => ({ addToast }));
vi.mock('./instanceStore', () => ({
  useInstanceStore: {
    getState: () => ({ instances: [], loadInstances }),
  },
}));

import { useModpacksStore } from './modpacksStore';

function report(overrides: Partial<ImportReport> = {}): ImportReport & { instanceName: string } {
  return {
    total_files: 2,
    installed: 1,
    failed: [],
    skipped: 0,
    warnings: [],
    instanceName: 'Pack',
    ...overrides,
  };
}

describe('CurseForge failed-file retry', () => {
  beforeEach(() => {
    invoke.mockReset();
    addToast.mockReset();
    loadInstances.mockClear();
    useModpacksStore.getState().resetInstall();
  });

  it('sends the camelCase projectId/fileId payload the Rust DTO expects', async () => {
    useModpacksStore.setState({
      installReport: report({
        failed: [
          {
            file_name: 'a.jar',
            project_id: 11,
            file_id: 22,
            reason: 'download failed',
            retryable: true,
          },
          {
            file_name: 'b.jar',
            project_id: 33,
            file_id: 44,
            reason: 'download failed',
            retryable: true,
          },
        ],
      }),
    });
    invoke.mockResolvedValue(report({ installed: 2, failed: [] }));

    await useModpacksStore.getState().retryFailedInstallFiles();

    expect(invoke).toHaveBeenCalledWith('cmd_retry_curseforge_import_files', {
      instanceName: 'Pack',
      files: [
        { projectId: 11, fileId: 22 },
        { projectId: 33, fileId: 44 },
      ],
    });
  });

  it('clears the report only when every retried file was installed', async () => {
    useModpacksStore.setState({
      installReport: report({
        failed: [
          {
            file_name: 'a.jar',
            project_id: 11,
            file_id: 22,
            reason: 'download failed',
            retryable: true,
          },
        ],
      }),
    });
    invoke.mockResolvedValue(report({ installed: 1, failed: [] }));

    await useModpacksStore.getState().retryFailedInstallFiles();

    expect(useModpacksStore.getState().installReport).toBeNull();
    expect(useModpacksStore.getState().retryingInstallFiles).toBe(false);
    expect(addToast).toHaveBeenCalledWith(t('modpacks.retry_done'), 'success');
    expect(loadInstances).toHaveBeenCalled();
  });

  it('keeps non-retryable failures and reports a partial retry', async () => {
    const brokenManifest = {
      file_name: 'broken-entry',
      project_id: null,
      file_id: null,
      reason: 'manifest has no projectID/fileID',
      retryable: false,
    };
    useModpacksStore.setState({
      installReport: report({
        failed: [
          {
            file_name: 'a.jar',
            project_id: 11,
            file_id: 22,
            reason: 'download failed',
            retryable: true,
          },
          brokenManifest,
        ],
      }),
    });
    const stillFailed = {
      file_name: 'a.jar',
      project_id: 11,
      file_id: 22,
      reason: 'still unavailable',
      retryable: true,
    };
    invoke.mockResolvedValue(report({ installed: 0, failed: [stillFailed] }));

    await useModpacksStore.getState().retryFailedInstallFiles();

    const updated = useModpacksStore.getState().installReport;
    expect(updated).not.toBeNull();
    expect(updated!.failed).toEqual([brokenManifest, stillFailed]);
    expect(addToast).toHaveBeenCalledWith(
      t('modpacks.retry_partial', { installed: '0', failed: '1' }),
      'warning',
    );
    expect(loadInstances).toHaveBeenCalled();
  });

  it('surfaces a retry request failure and releases the retry lock', async () => {
    useModpacksStore.setState({
      installReport: report({
        failed: [
          {
            file_name: 'a.jar',
            project_id: 11,
            file_id: 22,
            reason: 'download failed',
            retryable: true,
          },
        ],
      }),
    });
    invoke.mockRejectedValue(new Error('network down'));

    await useModpacksStore.getState().retryFailedInstallFiles();

    expect(useModpacksStore.getState().retryingInstallFiles).toBe(false);
    expect(useModpacksStore.getState().installReport).not.toBeNull();
    expect(addToast).toHaveBeenCalledWith(
      expect.stringContaining('network down'),
      'error',
    );
  });

  it('does not request a retry when no failed file has usable ids', async () => {
    useModpacksStore.setState({
      installReport: report({
        failed: [
          {
            file_name: 'unsigned.jar',
            project_id: 0,
            file_id: 0,
            reason: 'download failed',
            retryable: true,
          },
        ],
      }),
    });

    await useModpacksStore.getState().retryFailedInstallFiles();

    expect(invoke).not.toHaveBeenCalled();
    expect(useModpacksStore.getState().retryingInstallFiles).toBe(false);
    expect(useModpacksStore.getState().installReport).not.toBeNull();
  });
});

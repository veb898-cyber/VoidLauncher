import { beforeEach, describe, expect, it } from 'vitest';
import { useUpdateCheckStore, type UpdateInfo } from './updateCheckStore';

function update(oldFilename: string, name = oldFilename): UpdateInfo {
  return {
    name,
    oldVersion: '1.0.0',
    newVersion: '2.0.0',
    downloadUrl: 'https://example.invalid/file.jar',
    filename: 'file.jar',
    oldFilename,
    projectId: 'project',
    versionId: 'version',
    expectedSha1: '',
    provider: 'modrinth',
    fileId: null,
  };
}

describe('updateCheckStore', () => {
  beforeEach(() => {
    useUpdateCheckStore.setState({ checks: {} });
  });

  it('keeps a running check across remounts', () => {
    useUpdateCheckStore.getState().start('pack:mods');
    const entry = useUpdateCheckStore.getState().checks['pack:mods'];
    expect(entry.running).toBe(true);
    expect(entry.updates).toBeNull();
  });

  it('stores results and never keeps an empty dialog open', () => {
    useUpdateCheckStore.getState().start('pack:mods');
    useUpdateCheckStore.getState().succeed('pack:mods', [update('a.jar')]);
    expect(useUpdateCheckStore.getState().checks['pack:mods']).toEqual({
      running: false,
      updates: [update('a.jar')],
    });

    useUpdateCheckStore.getState().succeed('pack:mods', []);
    expect(useUpdateCheckStore.getState().checks['pack:mods']).toEqual({
      running: false,
      updates: null,
    });
  });

  it('removes only the applied file and dismisses the rest', () => {
    useUpdateCheckStore.getState().start('pack:mods');
    useUpdateCheckStore
      .getState()
      .succeed('pack:mods', [update('same-name.jar', 'Same Name'), update('other.jar', 'Same Name')]);
    useUpdateCheckStore.getState().remove('pack:mods', 'same-name.jar');
    expect(useUpdateCheckStore.getState().checks['pack:mods'].updates).toEqual([
      update('other.jar', 'Same Name'),
    ]);
    useUpdateCheckStore.getState().dismiss('pack:mods');
    expect(useUpdateCheckStore.getState().checks['pack:mods'].updates).toBeNull();
  });

  it('clears running state on failure', () => {
    useUpdateCheckStore.getState().start('pack:mods');
    useUpdateCheckStore.getState().fail('pack:mods');
    expect(useUpdateCheckStore.getState().checks['pack:mods']).toEqual({
      running: false,
      updates: null,
    });
  });
});

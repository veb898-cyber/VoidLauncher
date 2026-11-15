import { describe, expect, it } from 'vitest';
import {
  batchImportToastType,
  contentStatusKey,
  importPromptForResult,
  importStatusMessage,
  isLocalImportFailure,
  localImportErrorKey,
  localImportReloadHintKey,
  shouldOfferEnable,
  type LocalImportResult,
} from './localImport';

function result(overrides: Partial<LocalImportResult> = {}): LocalImportResult {
  return {
    status: 'installed',
    filename: 'Example.zip',
    contentType: 'resourcepack',
    enabled: true,
    activationSupported: true,
    requiresActivation: false,
    ...overrides,
  };
}

describe('local import result mapping', () => {
  it('reports a real install as success and a repeated import as a no-op', () => {
    expect(importStatusMessage(result())).toEqual({
      key: 'manager.local_status_installed',
      type: 'success',
    });
    expect(importStatusMessage(result({ status: 'alreadyInstalled' }))).toEqual({
      key: 'manager.local_status_already_installed',
      type: 'info',
    });
  });

  it('never labels a duplicate or a conflict as a successful reinstall', () => {
    expect(importStatusMessage(result({ status: 'duplicateContent' })).type).toBe('warning');
    expect(importStatusMessage(result({ status: 'conflict' })).type).toBe('warning');
    expect(importStatusMessage(result({ status: 'duplicateContent' })).key).toBe(
      'manager.local_status_duplicate',
    );
    expect(importStatusMessage(result({ status: 'conflict' })).key).toBe(
      'manager.local_status_conflict',
    );
  });

  it('offers activation for a resource pack that is installed but disabled', () => {
    const prompt = importPromptForResult(
      result({ status: 'alreadyInstalled', requiresActivation: true, enabled: false }),
      'C:\\packs\\Example.zip',
    );
    expect(prompt).toEqual({
      kind: 'activation',
      filename: 'Example.zip',
      name: 'Example.zip',
    });
  });

  it('does not offer activation for an already enabled pack', () => {
    expect(
      importPromptForResult(
        result({ status: 'installed', requiresActivation: false }),
        'C:\\packs\\Example.zip',
      ),
    ).toBeNull();
  });

  it('shows duplicate and conflict prompts with the source path', () => {
    const duplicate = importPromptForResult(
      result({ status: 'duplicateContent', duplicateOf: 'Old.zip' }),
      'C:\\packs\\Example.zip',
    );
    expect(duplicate?.kind).toBe('duplicate');
    expect(duplicate && 'sourcePath' in duplicate ? duplicate.sourcePath : '').toBe(
      'C:\\packs\\Example.zip',
    );

    const conflict = importPromptForResult(
      result({ status: 'conflict', conflictWith: 'Old.zip', existingSha256: 'abc' }),
      'C:\\packs\\Example.zip',
    );
    expect(conflict?.kind).toBe('conflict');
  });

  it('maps known backend failures to localized keys and falls back safely', () => {
    expect(localImportErrorKey('invalidArchive')).toBe('manager.local_error_invalidArchive');
    expect(localImportErrorKey('pathCollision')).toBe('manager.local_error_pathCollision');
    expect(localImportErrorKey('something-new')).toBe('manager.local_error_generic');
    expect(localImportErrorKey(undefined)).toBe('manager.local_error_generic');
    expect(isLocalImportFailure({ code: 'noRoot', message: 'missing marker' })).toBe(true);
    expect(isLocalImportFailure('plain error')).toBe(false);
  });

  it('separates enabled, disabled and shader activation states', () => {
    expect(contentStatusKey('mod', true)).toBe('manager.local_status_enabled');
    expect(contentStatusKey('mod', false)).toBe('manager.local_status_disabled');
    expect(contentStatusKey('resourcepack', false)).toBe('manager.local_status_not_enabled');
    expect(contentStatusKey('shader', true)).toBe('manager.local_status_shader');
    expect(shouldOfferEnable('resourcepack', false)).toBe(true);
    expect(shouldOfferEnable('resourcepack', true)).toBe(false);
    expect(shouldOfferEnable('mod', false)).toBe(false);
  });

  it('never labels a partial batch as a full success', () => {
    expect(batchImportToastType(3, 0)).toBe('success');
    expect(batchImportToastType(2, 1)).toBe('warning');
    expect(batchImportToastType(0, 3)).toBe('error');
  });

  it('shows the reload hint for resource packs and the in-game hint for shaders', () => {
    expect(localImportReloadHintKey('resourcepack')).toBe('manager.local_reload_hint');
    expect(localImportReloadHintKey('shader')).toBe('manager.local_shader_hint');
    expect(localImportReloadHintKey('mod')).toBeNull();
  });
});

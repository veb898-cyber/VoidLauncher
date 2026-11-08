/**
 * Local content import results and prompts.
 *
 * The backend distinguishes an actual install from a no-op, a duplicate and a
 * name conflict. Keeping that mapping here (instead of in ad-hoc component
 * branches) makes the UI honest: a repeated import must never look like a
 * successful reinstall.
 */

export type LocalImportStatus =
  | 'installed'
  | 'alreadyInstalled'
  | 'duplicateContent'
  | 'conflict';

export interface LocalImportResult {
  status: LocalImportStatus;
  filename: string;
  contentType: 'mod' | 'resourcepack' | 'shader';
  enabled: boolean;
  activationSupported: boolean;
  requiresActivation: boolean;
  duplicateOf?: string;
  conflictWith?: string;
  existingSha256?: string;
}

export interface LocalImportFailure {
  code?: string;
  message?: string;
}

export type LocalImportPrompt =
  | { kind: 'activation'; filename: string; name: string }
  | { kind: 'duplicate'; sourcePath: string; result: LocalImportResult }
  | { kind: 'conflict'; sourcePath: string; result: LocalImportResult };

const KNOWN_ERROR_CODES = new Set([
  'invalidArchive',
  'emptyArchive',
  'unsafePath',
  'symlink',
  'archiveTooLarge',
  'noRoot',
  'multipleRoots',
  'pathCollision',
  'ambiguousTarget',
  'modMustBeJar',
  'packMustBeZip',
  'notFound',
  'sourceInsideContent',
  'sourceContainsContent',
  'invalidLayout',
  'invalidConflictPolicy',
  'invalidInstance',
  'readFailed',
  'installFailed',
  'unsupportedType',
]);

export function isLocalImportFailure(error: unknown): error is LocalImportFailure {
  return (
    typeof error === 'object' &&
    error !== null &&
    (typeof (error as LocalImportFailure).code === 'string' ||
      typeof (error as LocalImportFailure).message === 'string')
  );
}

export function localImportErrorKey(code: string | undefined): string {
  return code && KNOWN_ERROR_CODES.has(code)
    ? `manager.local_error_${code}`
    : 'manager.local_error_generic';
}

/** The prompt the UI must show for a result, or `null` when no action is needed. */
export function importPromptForResult(
  result: LocalImportResult,
  sourcePath: string,
): LocalImportPrompt | null {
  if (result.status === 'conflict') {
    return { kind: 'conflict', sourcePath, result };
  }
  if (result.status === 'duplicateContent') {
    return { kind: 'duplicate', sourcePath, result };
  }
  if (
    (result.status === 'installed' || result.status === 'alreadyInstalled') &&
    result.contentType === 'resourcepack' &&
    result.requiresActivation
  ) {
    return { kind: 'activation', filename: result.filename, name: result.filename };
  }
  return null;
}

export interface ImportStatusMessage {
  key: string;
  type: 'success' | 'info' | 'warning';
}

/** Toast text for the result. Conflict/duplicate get a modal instead. */
export function importStatusMessage(result: LocalImportResult): ImportStatusMessage {
  switch (result.status) {
    case 'installed':
      return { key: 'manager.local_status_installed', type: 'success' };
    case 'alreadyInstalled':
      return { key: 'manager.local_status_already_installed', type: 'info' };
    case 'duplicateContent':
      return { key: 'manager.local_status_duplicate', type: 'warning' };
    case 'conflict':
      return { key: 'manager.local_status_conflict', type: 'warning' };
  }
}

/** Status shown in the detail pane: mods/packs are enabled or not; shader activation is in game. */
export function contentStatusKey(
  contentType: 'mod' | 'resourcepack' | 'shader',
  enabled: boolean,
): string {
  if (contentType === 'shader') return 'manager.local_status_shader';
  if (enabled) return 'manager.local_status_enabled';
  return contentType === 'resourcepack'
    ? 'manager.local_status_not_enabled'
    : 'manager.local_status_disabled';
}

/** A resource pack action is only meaningful when it is actually disabled. */
export function shouldOfferEnable(
  contentType: 'mod' | 'resourcepack' | 'shader',
  enabled: boolean,
): boolean {
  return contentType === 'resourcepack' && !enabled;
}

export function localImportReloadHintKey(contentType: 'mod' | 'resourcepack' | 'shader'): string | null {
  if (contentType === 'resourcepack') return 'manager.local_reload_hint';
  if (contentType === 'shader') return 'manager.local_shader_hint';
  return null;
}

export type BatchImportToastType = 'success' | 'warning' | 'error';

/**
 * A local/download batch is not one transaction: each file can succeed or
 * fail independently. The summary toast must not paint a batch with failures
 * as a full success — the text already shows both counts, so the colour must
 * agree with it.
 */
export function batchImportToastType(success: number, failed: number): BatchImportToastType {
  if (failed <= 0 && success > 0) return 'success';
  if (success > 0) return 'warning';
  return 'error';
}

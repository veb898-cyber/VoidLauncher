import { describe, expect, it } from 'vitest';
import {
  isServerConflict,
  isServerRunningError,
  parseServerError,
  serverErrorMessage,
} from './serverErrors';
import type { MessageKey } from './i18n';

const translate = (key: MessageKey, vars?: Record<string, string | number>) => {
  if (vars?.error !== undefined) return `${key}:${vars.error}`;
  return key;
};

describe('server error parsing', () => {
  it('reads a structured object from the Tauri bridge', () => {
    const parsed = parseServerError({ kind: 'instance_running' });
    expect(parsed.kind).toBe('instance_running');
    expect(serverErrorMessage({ kind: 'instance_running' }, translate)).toBe(
      'servers.error_running',
    );
  });

  it('reads a JSON-encoded structured error', () => {
    const error = JSON.stringify({ kind: 'conflict' });
    expect(isServerConflict(error)).toBe(true);
    expect(serverErrorMessage(error, translate)).toBe('servers.error_conflict');
  });

  it('keeps the technical message for an unknown structured error', () => {
    expect(
      serverErrorMessage({ kind: 'message', message: 'permission denied' }, translate),
    ).toBe('servers.action_error:permission denied');
  });

  it('falls back to the raw string without losing information', () => {
    expect(serverErrorMessage('plain failure', translate)).toBe(
      'servers.action_error:plain failure',
    );
  });

  it('recognizes both running and starting states', () => {
    expect(isServerRunningError({ kind: 'instance_running' })).toBe(true);
    expect(isServerRunningError(JSON.stringify({ kind: 'instance_starting' }))).toBe(true);
    expect(isServerRunningError({ kind: 'conflict' })).toBe(false);
  });
});

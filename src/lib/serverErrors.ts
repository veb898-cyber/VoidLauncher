import type { MessageKey } from './i18n';

type Translate = (key: MessageKey, vars?: Record<string, string | number>) => string;

export interface ParsedServerError {
  kind: string | null;
  message: string | null;
  raw: string;
}

function asRecord(value: unknown): Record<string, unknown> | null {
  return value && typeof value === 'object' ? (value as Record<string, unknown>) : null;
}

function parseRecord(value: unknown): ParsedServerError | null {
  const record = asRecord(value);
  if (!record) return null;
  const kind = typeof record.kind === 'string' ? record.kind : null;
  const message = typeof record.message === 'string' ? record.message : null;
  if (!kind && !message) return null;
  return { kind, message, raw: '' };
}

function rawText(value: unknown): string {
  if (typeof value === 'string') return value;
  if (value instanceof Error) return value.message;
  try {
    return JSON.stringify(value);
  } catch {
    return String(value);
  }
}

/**
 * Tauri may reject with a serialized object, a JSON string, or a plain
 * string. Normalize all three without assuming a particular bridge shape.
 */
export function parseServerError(error: unknown): ParsedServerError {
  const direct = parseRecord(error);
  if (direct) return { ...direct, raw: rawText(error) };

  if (typeof error === 'string') {
    const trimmed = error.trim();
    if (trimmed.startsWith('{') && trimmed.endsWith('}')) {
      try {
        const decoded = JSON.parse(trimmed);
        const parsed = parseRecord(decoded);
        if (parsed) return { ...parsed, raw: error };
      } catch {
        /* not a structured error, keep the raw string */
      }
    }
  }

  return { kind: null, message: null, raw: rawText(error) };
}

/** Localize a server write error; technical detail remains as a fallback. */
export function serverErrorMessage(error: unknown, t: Translate): string {
  const parsed = parseServerError(error);
  switch (parsed.kind) {
    case 'instance_running':
      return t('servers.error_running');
    case 'instance_starting':
      return t('servers.error_starting');
    case 'another_edit':
      return t('servers.error_another_edit');
    case 'conflict':
      return t('servers.error_conflict');
    default:
      return t('servers.action_error', {
        error: parsed.message ?? parsed.raw,
      });
  }
}

export function isServerConflict(error: unknown): boolean {
  return parseServerError(error).kind === 'conflict';
}

export function isServerRunningError(error: unknown): boolean {
  const kind = parseServerError(error).kind;
  return kind === 'instance_running' || kind === 'instance_starting';
}

/**
 * Pure log-view helpers shared by the Terminal viewer and its tests.
 *
 * The viewer keeps the whole filtered log in memory and only virtualizes the
 * DOM. Filtering must therefore never depend on the rendered range.
 */

export type LogSeverity = 'error' | 'warn' | 'debug' | '';

export interface LogLevelPreferences {
  error: boolean;
  warn: boolean;
  info: boolean;
}

export function getLineLevel(line: string): LogSeverity {
  const upper = line.toUpperCase();
  if (/\[ERROR\]|\/ERROR\]/.test(line)) return 'error';
  if (/\[WARN\]|\/WARN\]/.test(line)) return 'warn';
  if (/\[DEBUG\]|\/DEBUG\]/.test(line)) return 'debug';
  if (/\bEXCEPTION\b/.test(upper) || /\bFATAL\b/.test(upper)) return 'error';
  if (/exit code [1-9]/.test(line) || /exit code \d{2,}/.test(line)) return 'error';
  if (/FAILED/i.test(line) || /\bERROR\b/i.test(line)) return 'error';
  if (/WARNING/i.test(line)) return 'warn';
  return '';
}

export function getLineColor(level: LogSeverity): string {
  switch (level) {
    case 'error': return 'var(--color-danger)';
    case 'warn': return 'var(--color-warning)';
    case 'debug': return 'var(--text-tertiary)';
    default: return 'var(--text-primary)';
  }
}

function levelBucket(level: LogSeverity): keyof LogLevelPreferences {
  if (level === 'error') return 'error';
  if (level === 'warn') return 'warn';
  return 'info';
}

/**
 * Filter the complete in-memory log. `needle` is already trimmed/lowercased
 * by the caller or by this function; the original line text is returned
 * unchanged so rendering, copying and selection keep exact bytes.
 */
export function filterLogLines(
  lines: readonly string[],
  needle: string,
  levels: LogLevelPreferences,
): string[] {
  const query = needle.trim().toLowerCase();
  if (query === '' && levels.error && levels.warn && levels.info) {
    return [...lines];
  }
  return lines.filter((line) => {
    if (!levels[levelBucket(getLineLevel(line))]) return false;
    return query === '' || line.toLowerCase().includes(query);
  });
}

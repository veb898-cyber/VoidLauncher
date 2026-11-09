/**
 * Bug-report composition and client-side redaction.
 *
 * Backend-provided sections are already redacted; this pass protects the
 * user's own text and is the text the user actually previews/copies. The
 * backend remains the authoritative redactor for raw logs.
 */

export interface BugReportFields {
  summary: string;
  steps: string;
  expected: string;
  actual: string;
  extra: string;
}

export interface BugReportSection {
  id: string;
  title: string;
  content: string;
  enabled: boolean;
}

export interface BugReportLabels {
  title: string;
  summary: string;
  steps: string;
  expected: string;
  actual: string;
  extra: string;
}

export type BugReportField = keyof BugReportFields;
export type BugReportFieldErrors = Partial<Record<BugReportField, 'required'>>;

/**
 * Fields required by the repository's `.github/ISSUE_TEMPLATE/bug_report.yml`
 * (description, steps, expected and actual behavior). `extra` stays optional:
 * it is free-form context and automatic sections already cover the environment.
 */
export const REQUIRED_BUG_REPORT_FIELDS = [
  'summary',
  'steps',
  'expected',
  'actual',
] as const satisfies readonly BugReportField[];

/** Empty strings and whitespace-only values count as missing. */
export function bugReportFieldErrors(fields: BugReportFields): BugReportFieldErrors {
  const errors: BugReportFieldErrors = {};
  for (const field of REQUIRED_BUG_REPORT_FIELDS) {
    if (!fields[field].trim()) errors[field] = 'required';
  }
  return errors;
}

export function isBugReportComplete(fields: BugReportFields): boolean {
  return REQUIRED_BUG_REPORT_FIELDS.every((field) => fields[field].trim().length > 0);
}

const SECRET_KEY_PATTERN =
  'access_?token|refresh_?token|session_?id|client_?secret|api[._-]?key|apikey|password|passwd|authorization|cookie|set-cookie|prefetched|token|secret|credential|private_key';

const SECRET_KEY_WITHOUT_VALUE = new RegExp(
  `^\\s*["']?[A-Za-z0-9_.-]*(?:${SECRET_KEY_PATTERN})[A-Za-z0-9_.-]*["']?\\s*[:=]\\s*$`,
  'i',
);

function leadingIndent(line: string): number {
  return line.length - line.replace(/^[ \t]+/, '').length;
}

function looksLikeLogRecord(trimmed: string): boolean {
  return (
    trimmed.startsWith('[') ||
    trimmed.startsWith('at ') ||
    trimmed.startsWith('Caused by:') ||
    trimmed.startsWith('... ') ||
    /^(INFO|WARN|ERROR|DEBUG|TRACE)\b/.test(trimmed)
  );
}

function looksLikeKeyValue(trimmed: string): boolean {
  return /^[A-Za-z0-9_.-]+\s*[:=]\s*\S/.test(trimmed);
}

/**
 * Mask values that continue on the lines after a bare sensitive key:
 *
 *   password:
 *   synthetic-secret
 *
 * Continuation stops at a blank line, an independent key/value record or a
 * recognisable log record. Ambiguous lines are masked (fail closed). If the
 * continuation exceeds a bounded number of lines, the rest of the text is
 * masked rather than risking a leak.
 */
function maskPendingSecretLines(input: string): string {
  const lines = input.split('\n');
  const output: string[] = [];
  let pendingIndent: number | null = null;
  let pendingLines = 0;

  for (let index = 0; index < lines.length; index += 1) {
    const line = lines[index];
    if (pendingIndent !== null) {
      const trimmed = line.trim();
      if (trimmed === '') {
        pendingIndent = null;
        pendingLines = 0;
        output.push(line);
        continue;
      }
      const continues =
        leadingIndent(line) > pendingIndent ||
        (!looksLikeLogRecord(trimmed) && !looksLikeKeyValue(trimmed));
      if (continues) {
        pendingLines += 1;
        if (pendingLines > 32) {
          for (let rest = index; rest < lines.length; rest += 1) {
            output.push('[REDACTED]');
          }
          break;
        }
        output.push('[REDACTED]');
        continue;
      }
      pendingIndent = null;
      pendingLines = 0;
    }

    output.push(line);
    if (SECRET_KEY_WITHOUT_VALUE.test(line)) {
      pendingIndent = leadingIndent(line);
      pendingLines = 0;
    }
  }
  return output.join('\n');
}

/** Best-effort client redaction for user-edited text; mirrors the backend rules. */
export function redactReportText(input: string): string {
  let text = maskPendingSecretLines(input);

  // Multiline structural secret values (`"prefetched": { ...`): never leave
  // the remainder of the structure visible.
  text = text.replace(
    new RegExp(
      `("[A-Za-z0-9_.-]*(?:${SECRET_KEY_PATTERN})[A-Za-z0-9_.-]*"\\s*:\\s*)[\\[{][\\s\\S]*$`,
      'i',
    ),
    '$1[REDACTED]',
  );
  text = text.replace(
    new RegExp(
      `\\b([A-Za-z0-9_.-]*(?:${SECRET_KEY_PATTERN})[A-Za-z0-9_.-]*\\s*=\\s*)[\\[{][\\s\\S]*$`,
      'i',
    ),
    '$1[REDACTED]',
  );

  // JSON keys are case-insensitive, including prefixed/suffixed variants
  // such as `my_token` or `secret_key`.
  text = text.replace(
    new RegExp(
      `("[A-Za-z0-9_.-]*(?:${SECRET_KEY_PATTERN})[A-Za-z0-9_.-]*"\\s*:\\s*)("[^"]*"|'[^']*'|[^,}\\s]+)`,
      'gi',
    ),
    '$1"[REDACTED]"',
  );

  // JVM / Minecraft flags.
  text = text.replace(
    /(--(?:accessToken|access_token|refreshToken|refresh_token|sessionId|session_id|sessionToken|session_token|session|clientSecret|api[._-]?key|apikey|x-api-key|password|username|uuid|xuid)\b\s*(?:=|\s)\s*)("[^"]*"|'[^']*'|\S+)/gi,
    '$1[REDACTED]',
  );

  // -D properties carrying secrets.
  text = text.replace(
    /(-D[^\s=]*(?:token|secret|password|prefetched|authorization|cookie|api[._-]?key|apikey)[^\s=]*=)("[^"]*"|'[^']*'|\S+)/gi,
    '$1[REDACTED]',
  );

  // Generic name=value / name: value pairs (URLs, JSON-ish logs, etc.).
  // Allow a key to contain a sensitive substring (`my_token`, `secret_key`)
  // instead of requiring an exact field name.
  text = text.replace(
    new RegExp(
      `\\b([A-Za-z0-9_.-]*(?:${SECRET_KEY_PATTERN})[A-Za-z0-9_.-]*)(\\s*[:=]\\s*)("[^"]*"|'[^']*'|[^\\s&;,}\\]]+)`,
      'gi',
    ),
    '$1$2[REDACTED]',
  );

  // Headers and schemes.
  text = text.replace(/(authorization\s*[:=]\s*).*/gim, '$1[REDACTED]');
  text = text.replace(
    /(^|[^A-Za-z0-9_"])((?:set-)?cookie\s*[:=]\s*).*$/gim,
    '$1$2[REDACTED]',
  );
  text = text.replace(/\b(Bearer|Basic)\s+[A-Za-z0-9._~+/=-]+/gi, '$1 [REDACTED]');

  // Private-key blocks, including unterminated ones.
  text = text.replace(
    /-----BEGIN [^-]*PRIVATE KEY-----[\s\S]*?(?:-----END [^-]*PRIVATE KEY-----|$)/gi,
    '[REDACTED]',
  );

  // User home paths, including usernames with spaces, both separators, and
  // macOS `/Users/...` paths. The old pattern stopped at the first space and
  // left the surname (or the whole path) visible.
  text = text.replace(/[A-Za-z]:[\\/]+Users[\\/]+[^\\\r\n]+/gi, '<HOME>');
  text = text.replace(/\/(?:Users|home)\/[^/\r\n]+/g, '<HOME>');

  return text;
}

export function buildBugReport(
  fields: BugReportFields,
  sections: BugReportSection[],
  labels: BugReportLabels,
): string {
  const parts: string[] = [`# ${labels.title}`, ''];
  const addField = (label: string, value: string) => {
    if (!value.trim()) return;
    parts.push(`## ${label}`, redactReportText(value.trim()), '');
  };
  addField(labels.summary, fields.summary);
  addField(labels.steps, fields.steps);
  addField(labels.expected, fields.expected);
  addField(labels.actual, fields.actual);
  addField(labels.extra, fields.extra);

  for (const section of sections) {
    if (!section.enabled || !section.content.trim()) continue;
    parts.push(`## ${section.title}`, redactReportText(section.content.trim()), '');
  }
  return parts.join('\n').trimEnd();
}

/**
 * Resolve exactly what "Copy report" publishes. A manually edited preview
 * wins, but only after the same required-field validation as the form: an
 * incomplete form must never reach the copy/open-issue mechanism, even if the
 * preview text itself is non-empty. `null` means the report cannot be built.
 */
export function resolveReportForCopy(
  preview: string,
  fields: BugReportFields,
  sections: BugReportSection[],
  labels: BugReportLabels,
  previewEdited = true,
): string | null {
  if (!isBugReportComplete(fields)) return null;
  if (previewEdited && preview.trim()) return preview;
  return buildBugReport(fields, sections, labels);
}

export const BUG_REPORT_ISSUE_URL =
  'https://github.com/veb898-cyber/VoidLauncher/issues/new?template=bug_report.yml';

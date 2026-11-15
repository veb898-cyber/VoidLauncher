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
  'access_?token|refresh_?token|session_?id|client_?secret|api[._-]?key|apikey|password|passwd|authorization|cookie|set-cookie|prefetched|token|secret|credential|private_key|session';

// Quoted values must allow backslash escapes: `"password":"a\"b"` is one JSON
// string, not a value ending at the escaped quote. The old `"[^"]*"` pattern
// could leave the tail of such a secret visible.
const QUOTED_VALUE_SOURCE = String.raw`"(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*'`;

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
      `("[A-Za-z0-9_.-]*(?:${SECRET_KEY_PATTERN})[A-Za-z0-9_.-]*"\\s*:\\s*)(${QUOTED_VALUE_SOURCE}|[^,}\\s]+)`,
      'gi',
    ),
    '$1"[REDACTED]"',
  );

  // JVM / Minecraft flags.
  text = text.replace(
    /(--(?:accessToken|access_token|refreshToken|refresh_token|sessionId|session_id|sessionToken|session_token|session|clientSecret|api[._-]?key|apikey|x-api-key|password|username|uuid|xuid)\b\s*(?:=|\s)\s*)("(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*'|\S+)/gi,
    '$1[REDACTED]',
  );

  // -D properties carrying secrets.
  text = text.replace(
    /(-D[^\s=]*(?:token|secret|password|prefetched|authorization|cookie|api[._-]?key|apikey)[^\s=]*=)("(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*'|\S+)/gi,
    '$1[REDACTED]',
  );

  // Generic name=value / name: value pairs (URLs, JSON-ish logs, etc.).
  // Allow a key to contain a sensitive substring (`my_token`, `secret_key`)
  // instead of requiring an exact field name.
  text = text.replace(
    new RegExp(
      `\\b([A-Za-z0-9_.-]*(?:${SECRET_KEY_PATTERN})[A-Za-z0-9_.-]*)(\\s*[:=]\\s*)(${QUOTED_VALUE_SOURCE}|[^&;,}\\r\\n\\]]+)`,
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

export interface ParsedBugReport {
  fields: Pick<BugReportFields, 'summary' | 'steps' | 'expected' | 'actual' | 'extra'>;
  sections: Record<string, string>;
}

/**
 * Parse an edited preview back into the fields and sections that produced it.
 *
 * The in-app preview is editable, so a user may remove a path, a mod name or
 * another personal detail before publishing. Rebuilding the GitHub prefill
 * from the original form state would silently reintroduce that data. When the
 * required headings are present, this parser lets the prefill follow the
 * edited text; otherwise the caller opens the form without field prefill and
 * relies on the copied report.
 */
export function parseBugReportPreview(
  preview: string,
  labels: BugReportLabels,
  sectionTitles: Record<string, string>,
): ParsedBugReport | null {
  const headingToKey = new Map<string, string>();
  for (const key of ['summary', 'steps', 'expected', 'actual', 'extra'] as const) {
    headingToKey.set(labels[key], key);
  }
  for (const [id, title] of Object.entries(sectionTitles)) {
    headingToKey.set(title, id);
  }

  const result: Record<string, string> = {};
  let current: string | null = null;
  let buffer: string[] = [];
  const flush = () => {
    if (current) result[current] = buffer.join('\n').trim();
  };

  for (const line of preview.split(/\r?\n/)) {
    if (line.startsWith('## ')) {
      const heading = line.slice(3).trim();
      const key = headingToKey.get(heading);
      if (key && result[key] === undefined) {
        flush();
        current = key;
        buffer = [];
        continue;
      }
    }
    if (current) buffer.push(line);
  }
  flush();

  const fields = {
    summary: result.summary ?? '',
    steps: result.steps ?? '',
    expected: result.expected ?? '',
    actual: result.actual ?? '',
    extra: result.extra ?? '',
  };
  if (
    !fields.summary.trim() ||
    !fields.steps.trim() ||
    !fields.expected.trim() ||
    !fields.actual.trim()
  ) {
    return null;
  }

  const sections: Record<string, string> = {};
  for (const id of ['environment', 'instance', 'diagnostics', 'launcher', 'game', 'crash']) {
    if (result[id]) sections[id] = result[id];
  }
  return { fields, sections };
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

/**
 * Field ids declared in `.github/ISSUE_TEMPLATE/bug_report.yml`.
 *
 * GitHub uses the form element `id` as the canonical name for URL query
 * parameter prefills, so these names are the contract between the launcher and
 * the repository template. `githubTemplates.test.ts` keeps both sides in sync.
 */
export const BUG_REPORT_FORM_FIELDS = [
  'summary',
  'steps',
  'expected',
  'actual',
  'extra',
  'launcher_version',
  'windows_version',
  'minecraft_version',
  'loader',
  'environment',
  'instance',
  'diagnostics',
  'launcher_log',
  'game_log',
  'crash',
] as const;

export type BugReportFormField = (typeof BUG_REPORT_FORM_FIELDS)[number];

/** Values the in-app reporter can prefill in the GitHub issue form. */
export interface BugReportIssuePrefill {
  summary: string;
  steps: string;
  expected: string;
  actual: string;
  extra?: string;
  launcherVersion?: string | null;
  windowsVersion?: string | null;
  minecraftVersion?: string | null;
  loader?: string | null;
  environment?: string;
  instance?: string;
  diagnostics?: string;
  launcherLog?: string;
  gameLog?: string;
  crash?: string;
}

export interface BugReportIssueLink {
  url: string;
  /** True when a section was shortened or omitted to fit GitHub's URL limit. */
  shortened: boolean;
}

/**
 * GitHub rejects long issue URLs (the observed cutoff is between 6 and 7 KiB,
 * and the failure is a generic 5xx rather than a useful error). Keep a 6000
 * character ceiling so the URL also survives browsers, the Tauri opener and
 * proxies with a margin.
 */
export const GITHUB_ISSUE_URL_LIMIT = 6000;

const MAX_TITLE_LENGTH = 200;
const REQUIRED_FIELD_LIMIT = 1200;
const OPTIONAL_FIELD_LIMIT = 1600;
const TRUNCATION_MARKER = '\n[truncated to fit GitHub URL]';

const PREFILL_TO_FIELD: Record<keyof BugReportIssuePrefill, BugReportFormField> = {
  summary: 'summary',
  steps: 'steps',
  expected: 'expected',
  actual: 'actual',
  extra: 'extra',
  launcherVersion: 'launcher_version',
  windowsVersion: 'windows_version',
  minecraftVersion: 'minecraft_version',
  loader: 'loader',
  environment: 'environment',
  instance: 'instance',
  diagnostics: 'diagnostics',
  launcherLog: 'launcher_log',
  gameLog: 'game_log',
  crash: 'crash',
};

const REQUIRED_PREFILL_FIELDS: readonly (keyof BugReportIssuePrefill)[] = [
  'summary',
  'steps',
  'expected',
  'actual',
];

const OPTIONAL_PREFILL_FIELDS: readonly (keyof BugReportIssuePrefill)[] = [
  'extra',
  'launcherVersion',
  'windowsVersion',
  'minecraftVersion',
  'loader',
  'environment',
  'instance',
  'diagnostics',
  'launcherLog',
  'gameLog',
  'crash',
];

/**
 * Drop optional sections before shrinking the user-authored required fields.
 * Logs are the largest and least useful part when a URL has to be shortened;
 * the complete report is still copied to the clipboard by the modal.
 */
const OPTIONAL_DROP_ORDER: readonly (keyof BugReportIssuePrefill)[] = [
  'crash',
  'gameLog',
  'launcherLog',
  'diagnostics',
  'instance',
  'environment',
  'extra',
  'loader',
  'minecraftVersion',
  'windowsVersion',
  'launcherVersion',
];

function clampIssueValue(value: string, limit: number): { text: string; truncated: boolean } {
  const redacted = redactReportText(value.trim());
  if (redacted.length <= limit) return { text: redacted, truncated: false };
  const withoutPreviousMarker = redacted.endsWith(TRUNCATION_MARKER)
    ? redacted.slice(0, -TRUNCATION_MARKER.length)
    : redacted;
  const keep = Math.max(0, limit - TRUNCATION_MARKER.length);
  return {
    text: `${withoutPreviousMarker.slice(0, keep).trimEnd()}${TRUNCATION_MARKER}`,
    truncated: true,
  };
}

function buildIssueUrl(
  values: Partial<Record<BugReportFormField, string>>,
  title: string,
): string {
  const url = new URL(BUG_REPORT_ISSUE_URL);
  url.searchParams.set('title', title);
  for (const field of BUG_REPORT_FORM_FIELDS) {
    const value = values[field];
    if (value) url.searchParams.set(field, value);
  }
  return url.toString();
}

/**
 * Build a GitHub "new issue" URL that prefills the bug-report form.
 *
 * GitHub issue forms support field-id query parameters (`?summary=...`), but
 * the server rejects overly long issue URLs. Large logs are therefore
 * shortened/dropped in a deterministic order; required fields are kept last.
 * Every value passes through `redactReportText` again, so a manually edited
 * preview or a pasted secret cannot bypass URL redaction.
 */
export function buildBugReportIssueUrl(prefill: BugReportIssuePrefill): BugReportIssueLink {
  const safeSummary = redactReportText(prefill.summary.trim())
    .replace(/\s+/g, ' ')
    .trim();
  const fullTitle = safeSummary ? `[Bug]: ${safeSummary}` : '[Bug]: ';
  const title = fullTitle.slice(0, MAX_TITLE_LENGTH);
  let shortened = fullTitle.length > title.length;

  const values: Partial<Record<BugReportFormField, string>> = {};
  for (const key of REQUIRED_PREFILL_FIELDS) {
    const raw = prefill[key];
    if (typeof raw !== 'string' || !raw.trim()) continue;
    const result = clampIssueValue(raw, REQUIRED_FIELD_LIMIT);
    values[PREFILL_TO_FIELD[key]] = result.text;
    if (result.truncated) shortened = true;
  }
  for (const key of OPTIONAL_PREFILL_FIELDS) {
    const raw = prefill[key];
    if (typeof raw !== 'string' || !raw.trim()) continue;
    const result = clampIssueValue(raw, OPTIONAL_FIELD_LIMIT);
    values[PREFILL_TO_FIELD[key]] = result.text;
    if (result.truncated) shortened = true;
  }

  let url = buildIssueUrl(values, title);
  if (url.length <= GITHUB_ISSUE_URL_LIMIT) return { url, shortened };

  for (const key of OPTIONAL_DROP_ORDER) {
    const field = PREFILL_TO_FIELD[key];
    if (!(field in values)) continue;
    delete values[field];
    shortened = true;
    url = buildIssueUrl(values, title);
    if (url.length <= GITHUB_ISSUE_URL_LIMIT) return { url, shortened };
  }

  for (const limit of [800, 400, 200, 80]) {
    for (const key of REQUIRED_PREFILL_FIELDS) {
      const field = PREFILL_TO_FIELD[key];
      const current = values[field];
      if (!current) continue;
      const result = clampIssueValue(current, limit);
      if (result.text !== current) shortened = true;
      values[field] = result.text;
    }
    url = buildIssueUrl(values, title);
    if (url.length <= GITHUB_ISSUE_URL_LIMIT) return { url, shortened };
  }

  // Extremely long required text cannot fit a prefilled URL. The form still
  // opens with the title, and the complete report is on the clipboard.
  return { url: buildIssueUrl({}, title), shortened: true };
}

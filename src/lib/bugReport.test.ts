import { describe, expect, it } from 'vitest';
import { t } from './i18n';
import { useLanguageStore } from '../stores/languageStore';
import {
  buildBugReport,
  BUG_REPORT_ISSUE_URL,
  redactReportText,
  resolveReportForCopy,
  type BugReportFields,
  type BugReportLabels,
  type BugReportSection,
} from './bugReport';

const labels: BugReportLabels = {
  title: 'Bug report',
  summary: 'Summary',
  steps: 'Steps',
  expected: 'Expected',
  actual: 'Actual',
  extra: 'Additional info',
};

const fields: BugReportFields = {
  summary: 'Launcher crashes',
  steps: '1. Launch\n2. Wait',
  expected: 'The game starts',
  actual: 'Nothing happens',
  extra: 'Instance: Pack',
};

describe('bug report redaction', () => {
  it('removes tokens, refresh tokens, session ids and Ely.by prefetched data', () => {
    const input = [
      '--accessToken abc.def.ghi',
      '--session legacy-session-secret',
      '{"refresh_token":"refresh-secret","sessionId":"session-secret"}',
      '-Dauthlibinjector.yggdrasil.prefetched=eyJhY2Nlc3NUb2tlbiI6IngifQ==',
      'Authorization: Bearer bearer-secret',
    ].join('\n');
    const output = redactReportText(input);
    for (const secret of [
      'abc.def.ghi',
      'legacy-session-secret',
      'refresh-secret',
      'session-secret',
      'eyJhY2Nlc3NUb2tlbiI6IngifQ==',
      'bearer-secret',
    ]) {
      expect(output).not.toContain(secret);
    }
    expect(output).toContain('[REDACTED]');
  });

  it('redacts JVM properties and JSON keys regardless of case', () => {
    const output = redactReportText(
      '{"AccessToken":"one","CLIENT_SECRET":"two","Cookie":"three"}\n-Dsome.password=four',
    );
    expect(output).not.toContain('one');
    expect(output).not.toContain('two');
    expect(output).not.toContain('three');
    expect(output).not.toContain('four');
  });

  it('redacts API keys in JSON, headers, queries and JVM properties', () => {
    const output = redactReportText(
      [
        '{"apiKey":"json-api-key","api_key":"json-snake-key"}',
        'X-Api-Key: header-api-key',
        'https://example.invalid/?api_key=query-api-key&page=1',
        '-Dapi.key=jvm-api-key',
        'ordinary_key=keep-this-value',
      ].join('\n'),
    );
    for (const secret of [
      'json-api-key',
      'json-snake-key',
      'header-api-key',
      'query-api-key',
      'jvm-api-key',
    ]) {
      expect(output).not.toContain(secret);
    }
    expect(output).toContain('keep-this-value');
    expect(output).toContain('page=1');
  });

  it('redacts multiline secret continuations without eating following records', () => {
    const output = redactReportText(
      [
        'password:',
        'synthetic-password',
        'version: 1.2.3',
        'secret:',
        '  synthetic-secret-line-1',
        '  synthetic-secret-line-2',
        'api_key:',
        'synthetic-api-key',
        '',
        '[INFO] independent record',
      ].join('\n'),
    );
    for (const secret of [
      'synthetic-password',
      'synthetic-secret-line-1',
      'synthetic-secret-line-2',
      'synthetic-api-key',
    ]) {
      expect(output).not.toContain(secret);
    }
    expect(output).toContain('version: 1.2.3');
    expect(output).toContain('[INFO] independent record');
  });

  it('masks the rest of an unbounded multiline secret continuation', () => {
    const input = ['password:', ...Array.from({ length: 40 }, (_, i) => `secret-${i}`)].join('\n');
    const output = redactReportText(input);
    expect(output).not.toContain('secret-0');
    expect(output).not.toContain('secret-39');
    expect(output).toContain('password:');
  });

  it('redacts prefixed key variants and cookie headers', () => {
    const output = redactReportText(
      '{"id_token":"id-secret","my_secret":"custom-secret"}\n[INFO] Cookie: session=abc; theme=dark',
    );
    expect(output).not.toContain('id-secret');
    expect(output).not.toContain('custom-secret');
    expect(output).not.toContain('theme=dark');
  });

  it('masks home paths and removes private-key material', () => {
    const output = redactReportText(
      'C:\\Users\\alice\\AppData\\Roaming\\VoidLauncher\n-----BEGIN PRIVATE KEY-----\nsecret\n-----END PRIVATE KEY-----',
    );
    expect(output).not.toContain('alice');
    expect(output).not.toContain('secret');
    expect(output).toContain('<HOME>');
  });

  it('masks home paths with spaces and both separators', () => {
    const output = redactReportText(
      [
        'C:\\Users\\John Doe\\AppData\\Roaming\\VoidLauncher',
        'C:/Users/Jane Doe/AppData/Local',
        '/Users/Alice Smith/Library/Application Support',
        '/home/Bob Smith/.config',
      ].join('\n'),
    );
    expect(output).not.toContain('John Doe');
    expect(output).not.toContain('Jane Doe');
    expect(output).not.toContain('Alice Smith');
    expect(output).not.toContain('Bob Smith');
    expect(output).toContain('<HOME>');
  });

  it('omits a multiline structural secret from the visible tail', () => {
    const output = redactReportText('{"prefetched": {\n"accessToken": "secret"\n}');
    expect(output).not.toContain('secret');
    expect(output).toContain('[REDACTED]');
  });
});

describe('bug report composition', () => {
  it('builds a report from fields and enabled sections only', () => {
    const sections: BugReportSection[] = [
      { id: 'environment', title: 'Environment', content: 'OS: Windows', enabled: true },
      { id: 'launcher_log', title: 'Launcher log', content: 'line', enabled: false },
    ];
    const report = buildBugReport(fields, sections, labels);
    expect(report).toContain('# Bug report');
    expect(report).toContain('## Summary');
    expect(report).toContain('Launcher crashes');
    expect(report).toContain('## Environment');
    expect(report).toContain('OS: Windows');
    expect(report).not.toContain('Launcher log');
    expect(report).not.toContain('line');
  });

  it('preview text is the exact text that would be copied', () => {
    const report = buildBugReport(fields, [], labels);
    expect(report).toBe(buildBugReport(fields, [], labels));
    expect(report.trim()).toBe(report);
    expect(resolveReportForCopy('', fields, [], labels)).toBe(report);
    expect(resolveReportForCopy('edited preview', fields, [], labels)).toBe('edited preview');
  });

  it('rebuilds an unedited preview after sections change', () => {
    const sections: BugReportSection[] = [
      { id: 'environment', title: 'Environment', content: 'OS: Windows', enabled: true },
      { id: 'launcher_log', title: 'Launcher log', content: 'secret log', enabled: false },
    ];
    const copied = resolveReportForCopy(
      '# Stale report\n\n## Launcher log\n\nsecret log',
      fields,
      sections,
      labels,
      false,
    );
    expect(copied).not.toContain('Launcher log');
    expect(copied).not.toContain('secret log');
    expect(copied).toContain('## Environment');
  });

  it('copies a manually edited preview verbatim', () => {
    const sections: BugReportSection[] = [];
    expect(
      resolveReportForCopy('manually edited text', fields, sections, labels, true),
    ).toBe('manually edited text');
  });

  it('refuses to copy a report without the required summary', () => {
    expect(
      resolveReportForCopy('', { ...fields, summary: '   ' }, [], labels),
    ).toBeNull();
  });

  it('does not include a secret pasted into a user field', () => {
    const report = buildBugReport(
      { ...fields, summary: '--accessToken synthetic-secret' },
      [],
      labels,
    );
    expect(report).not.toContain('synthetic-secret');
    expect(report).toContain('[REDACTED]');
  });

  it('opens the bug-report issue template without sending anything', () => {
    expect(BUG_REPORT_ISSUE_URL).toContain('/issues/new');
    expect(BUG_REPORT_ISSUE_URL).toContain('template=bug_report.yml');
  });
});

describe('bug report localization', () => {
  it('switches the dialog action labels with the interface language', () => {
    useLanguageStore.setState({ language: 'en' });
    expect(t('bug.open')).toBe('Report a bug');
    expect(t('bug.copy')).toBe('Copy report');
    expect(t('bug.open_github')).toBe('Open GitHub issue');
    expect(t('bug.privacy_warning')).toContain('never sends an issue automatically');

    useLanguageStore.setState({ language: 'ru' });
    expect(t('bug.open')).toBe('Сообщить об ошибке');
    expect(t('bug.copy')).toBe('Скопировать отчёт');
    expect(t('bug.open_github')).toContain('GitHub');
    expect(t('bug.privacy_warning')).toContain('никогда не отправляет');
    useLanguageStore.setState({ language: 'en' });
  });
});

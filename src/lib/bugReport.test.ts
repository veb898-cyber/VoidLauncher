import { describe, expect, it } from 'vitest';
import { t } from './i18n';
import { useLanguageStore } from '../stores/languageStore';
import { useToastSettingsStore } from '../stores/toastSettingsStore';
import {
  buildBugReport,
  buildBugReportIssueUrl,
  BUG_REPORT_ISSUE_URL,
  bugReportFieldErrors,
  GITHUB_ISSUE_URL_LIMIT,
  isBugReportComplete,
  parseBugReportPreview,
  redactReportText,
  resolveReportForCopy,
  type BugReportFields,
  type BugReportIssuePrefill,
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

  it('redacts bare session values and escaped quoted secrets', () => {
    const input = [
      'session=legacy-session-value',
      String.raw`{"session":"json-session-value"}`,
      String.raw`{"password":"escaped\"secret-tail"}`,
      String.raw`--accessToken "quoted\"token-secret"`,
      String.raw`-Dsome.password='single\'quoted-secret'`,
    ].join('\n');
    const output = redactReportText(input);
    for (const secret of [
      'legacy-session-value',
      'json-session-value',
      'secret-tail',
      'token-secret',
      'quoted-secret',
    ]) {
      expect(output).not.toContain(secret);
    }
    expect(output).toContain('[REDACTED]');
  });

  it('redacts unquoted secret values that contain spaces', () => {
    const output = redactReportText(
      [
        'password: correct horse battery staple',
        'api_key=query-secret&page=1',
        'version: 1.2.3',
      ].join('\n'),
    );
    for (const secret of ['correct', 'horse', 'battery', 'staple', 'query-secret']) {
      expect(output).not.toContain(secret);
    }
    expect(output).toContain('page=1');
    expect(output).toContain('version: 1.2.3');
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

  it('requires steps, expected and actual behavior as well', () => {
    for (const field of ['steps', 'expected', 'actual'] as const) {
      expect(resolveReportForCopy('', { ...fields, [field]: ' \n\t ' }, [], labels)).toBeNull();
    }
  });

  it('allows a complete report with the optional extra field empty', () => {
    const report = resolveReportForCopy('', { ...fields, extra: '' }, [], labels);
    expect(report).toContain('## Summary');
    expect(report).not.toContain('## Additional info');
  });

  it('does not accept an edited preview while required fields are missing', () => {
    expect(
      resolveReportForCopy('manually edited text', { ...fields, steps: '' }, [], labels, true),
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

  it('parses an edited preview back into fields and sections', () => {
    const sections: BugReportSection[] = [
      { id: 'environment', title: 'Environment', content: 'OS: Windows', enabled: true },
      { id: 'game', title: 'Game log', content: 'line one', enabled: true },
    ];
    const report = buildBugReport(fields, sections, labels).replace(
      'The game starts',
      'The edited expectation',
    );
    const parsed = parseBugReportPreview(report, labels, {
      environment: 'Environment',
      game: 'Game log',
    });
    expect(parsed).not.toBeNull();
    expect(parsed?.fields.summary).toBe(fields.summary);
    expect(parsed?.fields.expected).toBe('The edited expectation');
    expect(parsed?.sections.environment).toBe('OS: Windows');
    expect(parsed?.sections.game).toBe('line one');
  });

  it('refuses to parse a preview without the required headings', () => {
    expect(parseBugReportPreview('just some edited text', labels, {})).toBeNull();
    const report = buildBugReport(fields, [], labels).replace('## Steps\n', '');
    expect(parseBugReportPreview(report, labels, {})).toBeNull();
  });

  it('does not treat unknown markdown headings inside a section as separators', () => {
    const sections: BugReportSection[] = [
      { id: 'game', title: 'Game log', content: 'before\n## not a real heading\nafter', enabled: true },
    ];
    const report = buildBugReport(fields, sections, labels);
    const parsed = parseBugReportPreview(report, labels, { game: 'Game log' });
    expect(parsed?.sections.game).toContain('## not a real heading');
    expect(parsed?.sections.game).toContain('after');
  });

  it('opens the bug-report issue template without sending anything', () => {
    expect(BUG_REPORT_ISSUE_URL).toContain('/issues/new');
    expect(BUG_REPORT_ISSUE_URL).toContain('template=bug_report.yml');
  });
});

describe('bug report validation', () => {
  it('reports every empty required field, including whitespace-only values', () => {
    expect(
      bugReportFieldErrors({
        summary: ' ',
        steps: '\n\t',
        expected: 'The game should start',
        actual: '',
        extra: 'kept as optional',
      }),
    ).toEqual({ summary: 'required', steps: 'required', actual: 'required' });
    expect(isBugReportComplete(fields)).toBe(true);
    expect(isBugReportComplete({ ...fields, actual: '   ' })).toBe(false);
  });

  it('does not depend on Toast settings or any UI notification state', () => {
    // Pure validation: no toast/store imports and no side effects.
    useToastSettingsStore.setState({ success: false, warning: false, error: false });
    expect(isBugReportComplete({ ...fields, expected: '' })).toBe(false);
    expect(
      bugReportFieldErrors({ ...fields, expected: '' }).expected,
    ).toBe('required');
    useToastSettingsStore.setState({ success: true, warning: true, error: true });
  });
});

describe('GitHub issue prefill', () => {
  const prefill: BugReportIssuePrefill = {
    summary: 'Launcher & game crash',
    steps: '1. Open the launcher\n2. Click Play\n3. Wait',
    expected: 'The game starts',
    actual: 'Nothing happens',
    extra: 'Instance: Test Pack',
    launcherVersion: '1.5.0',
    windowsVersion: 'Windows 11 23H2, x64',
    minecraftVersion: '1.21.1',
    loader: 'Fabric',
    environment: 'VoidLauncher: 1.5.0\nOS: Windows 11',
    instance: 'Instance: Test Pack\nMinecraft: 1.21.1\nLoader: Fabric',
    diagnostics: 'Network: 4/4',
    launcherLog: 'INFO launcher started',
    gameLog: 'INFO game started',
    crash: 'Verdict: likely',
  };

  it('uses the bug-report form ids and URL-encodes every value', () => {
    const { url, shortened } = buildBugReportIssueUrl(prefill);
    expect(shortened).toBe(false);
    const parsed = new URL(url);
    expect(parsed.origin).toBe('https://github.com');
    expect(parsed.pathname).toBe('/veb898-cyber/VoidLauncher/issues/new');
    expect(parsed.searchParams.get('template')).toBe('bug_report.yml');
    expect(parsed.searchParams.get('title')).toBe('[Bug]: Launcher & game crash');
    for (const [field, value] of Object.entries({
      summary: prefill.summary,
      steps: prefill.steps,
      expected: prefill.expected,
      actual: prefill.actual,
      extra: prefill.extra,
      launcher_version: prefill.launcherVersion,
      windows_version: prefill.windowsVersion,
      minecraft_version: prefill.minecraftVersion,
      loader: prefill.loader,
      environment: prefill.environment,
      instance: prefill.instance,
      diagnostics: prefill.diagnostics,
      launcher_log: prefill.launcherLog,
      game_log: prefill.gameLog,
      crash: prefill.crash,
    })) {
      expect(parsed.searchParams.get(field), field).toBe(value);
    }
  });

  it('redacts secrets before they reach the URL', () => {
    const { url } = buildBugReportIssueUrl({
      ...prefill,
      summary: '--accessToken synthetic-access-secret',
      extra: 'password: synthetic-password',
      environment: 'Authorization: Bearer synthetic-bearer-secret',
    });
    const parsed = new URL(url);
    const visible = [
      parsed.searchParams.get('title'),
      parsed.searchParams.get('summary'),
      parsed.searchParams.get('extra'),
      parsed.searchParams.get('environment'),
    ].join('\n');
    for (const secret of [
      'synthetic-access-secret',
      'synthetic-password',
      'synthetic-bearer-secret',
    ]) {
      expect(visible).not.toContain(secret);
    }
    expect(visible).toContain('[REDACTED]');
  });

  it('redacts session and escaped quoted values before URL encoding', () => {
    const { url } = buildBugReportIssueUrl({
      ...prefill,
      extra: 'session=legacy-session-url-secret',
      environment: String.raw`{"password":"escaped\"url-secret-tail"}`,
    });
    const parsed = new URL(url);
    const visible = [
      parsed.searchParams.get('extra'),
      parsed.searchParams.get('environment'),
    ].join('\n');
    expect(visible).not.toContain('legacy-session-url-secret');
    expect(visible).not.toContain('url-secret-tail');
    expect(visible).toContain('[REDACTED]');
  });

  it('keeps required fields and stays under GitHub URL limit with huge logs', () => {
    const huge = (label: string) => `${label}\n${'x'.repeat(5000)}`;
    const { url, shortened } = buildBugReportIssueUrl({
      ...prefill,
      environment: huge('environment'),
      instance: huge('instance'),
      diagnostics: huge('diagnostics'),
      launcherLog: huge('launcher'),
      gameLog: huge('game'),
      crash: huge('crash'),
    });
    expect(shortened).toBe(true);
    expect(url.length).toBeLessThanOrEqual(GITHUB_ISSUE_URL_LIMIT);
    const parsed = new URL(url);
    expect(parsed.searchParams.get('summary')).toBe(prefill.summary);
    expect(parsed.searchParams.get('steps')).toBe(prefill.steps);
    expect(parsed.searchParams.get('expected')).toBe(prefill.expected);
    expect(parsed.searchParams.get('actual')).toBe(prefill.actual);
    expect(parsed.searchParams.get('crash')).toBeNull();
  });

  it('does not add empty optional fields', () => {
    const { url } = buildBugReportIssueUrl({
      summary: 'Only the required fields',
      steps: '1. Step',
      expected: 'It works',
      actual: 'It fails',
      crash: '   ',
    });
    const parsed = new URL(url);
    expect(parsed.searchParams.has('crash')).toBe(false);
    expect(parsed.searchParams.has('extra')).toBe(false);
    expect(parsed.searchParams.get('title')).toBe('[Bug]: Only the required fields');
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

import { describe, expect, it, vi } from 'vitest';
import {
  buildModListEntries,
  escapeHtml,
  exportExtension,
  generateCustomModList,
  generateModList,
  previewKind,
  templateEscapeForExtension,
  templateForFormat,
  type ModListEntry,
  type ModListExportOptions,
  type ModListSourceItem,
} from './modListExport';
import { copyModListText } from './modListExportFile';

const ALL_OFF: ModListExportOptions = {
  version: false,
  authors: false,
  url: false,
  filename: false,
  enabled: false,
};

function source(overrides: Partial<ModListSourceItem> = {}): ModListSourceItem {
  return {
    filename: 'alpha.jar',
    name: 'Alpha',
    version: '1.0.0',
    version_known: true,
    provider: 'Modrinth',
    enabled: true,
    project_id: 'alpha123',
    mod_id: 'alpha_mod',
    authors: ['Alice', 'Bob'],
    homepage: null,
    ...overrides,
  };
}

function entry(overrides: Partial<ModListEntry> = {}): ModListEntry {
  return {
    name: 'Alpha',
    modId: 'alpha_mod',
    url: 'https://modrinth.com/mod/alpha123',
    version: '1.0.0',
    authors: ['Alice', 'Bob'],
    filename: 'alpha.jar',
    enabled: true,
    ...overrides,
  };
}

describe('mod list data preparation', () => {
  it('exports every installed mod when nothing is selected', () => {
    const entries = buildModListEntries([
      source({ filename: 'b.jar', name: 'Beta' }),
      source({ filename: 'a.jar', name: 'Alpha' }),
    ]);
    expect(entries.map((item) => item.name)).toEqual(['Alpha', 'Beta']);
  });

  it('always exports the complete list, not a selection subset', () => {
    const entries = buildModListEntries([
      source({ filename: 'a.jar', name: 'Alpha' }),
      source({ filename: 'b.jar', name: 'Beta' }),
      source({ filename: 'c.jar', name: 'Charlie', enabled: false }),
    ]);
    expect(entries.map((item) => item.filename)).toEqual([
      'a.jar',
      'b.jar',
      'c.jar',
    ]);
  });

  it('keeps disabled mods and reports their state', () => {
    const entries = buildModListEntries([
      source({ filename: 'off.jar', name: 'Off', enabled: false }),
    ]);
    expect(entries).toHaveLength(1);
    expect(entries[0].enabled).toBe(false);
    expect(generateModList(entries, {
      format: 'text',
      options: { ...ALL_OFF, enabled: true },
      customTemplate: '',
      customExtension: 'txt',
    })).toBe('Off [disabled]');
  });

  it('sorts by display name, not by filename or installation order', () => {
    const entries = buildModListEntries([
      source({ filename: 'z.jar', name: 'zeta' }),
      source({ filename: 'a.jar', name: 'Alpha' }),
      source({ filename: 'm.jar', name: 'beta' }),
    ]);
    expect(entries.map((item) => item.name)).toEqual(['Alpha', 'beta', 'zeta']);
  });

  it('handles missing URL, authors, version and mod id without inventing values', () => {
    const entries = buildModListEntries([
      source({
        filename: 'local-mod.jar',
        name: '',
        version: '',
        version_known: false,
        provider: 'Local',
        project_id: null,
        mod_id: null,
        authors: null,
        homepage: null,
      }),
    ]);
    expect(entries[0]).toEqual({
      name: 'local-mod',
      modId: '',
      url: '',
      version: '',
      authors: [],
      filename: 'local-mod.jar',
      enabled: true,
    });
  });

  it('drops build-placeholder versions even without the explicit flag', () => {
    const entries = buildModListEntries([
      source({ version: 'Unknown', version_known: undefined }),
      source({ filename: 'placeholder.jar', name: 'Placeholder', version: '${version}' }),
    ]);
    expect(entries.map((item) => item.version)).toEqual(['', '']);
  });

  it('derives provider URLs only from verified project ids', () => {
    const entries = buildModListEntries([
      source({ filename: 'cf.jar', name: 'CF', provider: 'CurseForge', project_id: '394468' }),
      source({ filename: 'home.jar', name: 'Home', provider: 'Local', project_id: null, homepage: 'https://example.com/home' }),
      source({ filename: 'bad.jar', name: 'Bad', provider: 'Modrinth', project_id: '../escape' }),
      source({ filename: 'nohost.jar', name: 'NoHost', provider: 'Local', project_id: null, homepage: 'https:///' }),
      source({ filename: 'slug.jar', name: 'Slug', provider: 'Modrinth', project_id: null, mod_id: null }),
    ]);
    expect(entries.find((item) => item.filename === 'cf.jar')?.url).toBe(
      'https://www.curseforge.com/minecraft/mc-mods/394468',
    );
    expect(entries.find((item) => item.filename === 'home.jar')?.url).toBe(
      'https://example.com/home',
    );
    expect(entries.find((item) => item.filename === 'bad.jar')?.url).toBe('');
    expect(entries.find((item) => item.filename === 'nohost.jar')?.url).toBe(
      '',
    );
    expect(entries.find((item) => item.filename === 'slug.jar')?.modId).toBe(
      '',
    );
    expect(entries.find((item) => item.filename === 'slug.jar')?.url).toBe('');
    // A verified slug is the project id, never the internal mod id.
    expect(entries.find((item) => item.filename === 'cf.jar')?.modId).toBe(
      'alpha_mod',
    );
  });
});

describe('HTML generation', () => {
  it('escapes names and attributes and renders optional fields', () => {
    const html = generateModList(
      [
        entry({
          name: 'Sodium & Co <script>',
          url: 'https://example.com/mod?a=1&b=2',
          authors: ['Alice & Bob'],
          filename: 'sodium <test>.jar',
        }),
      ],
      {
        format: 'html',
        options: { version: true, authors: true, url: true, filename: true, enabled: true },
        customTemplate: '',
        customExtension: 'html',
        instanceName: 'My <Pack>',
      },
    );
    expect(html).toContain('<!DOCTYPE html>');
    expect(html).toContain('<meta charset="utf-8">');
    expect(html).toContain('My &lt;Pack&gt; — mod list');
    expect(html).toContain(
      '<a href="https://example.com/mod?a=1&amp;b=2">Sodium &amp; Co &lt;script&gt;</a>',
    );
    expect(html).toContain('[1.0.0]');
    expect(html).toContain('by Alice &amp; Bob');
    expect(html).toContain('(sodium &lt;test&gt;.jar)');
    expect(html).toContain('[enabled]');
    expect(html).not.toContain('<script>');
    expect(escapeHtml('<>&"\'')).toBe('&lt;&gt;&amp;&quot;&#39;');
  });

  it('leaves the name unlinked when URL is not selected or absent', () => {
    const html = generateModList([entry({ url: '' })], {
      format: 'html',
      options: { ...ALL_OFF, url: true },
      customTemplate: '',
      customExtension: 'html',
      instanceName: 'Pack',
    });
    expect(html).toContain('<li>Alpha</li>');
    expect(html).not.toContain('<a href=');
  });
});

describe('Markdown and plain text generation', () => {
  it('renders a clickable Markdown link and escapes punctuation', () => {
    const markdown = generateModList(
      [entry({ name: 'Alpha [Beta]', url: 'https://example.com/a_(b)' })],
      {
        format: 'markdown',
        options: { ...ALL_OFF, url: true, version: true },
        customTemplate: '',
        customExtension: 'md',
      },
    );
    expect(markdown).toBe(
      '- [Alpha \\[Beta\\]](https://example.com/a_%28b%29) [1.0.0]',
    );
  });

  it('writes one readable line per mod in plain text', () => {
    const text = generateModList(
      [entry({ version: '', authors: [] })],
      {
        format: 'text',
        options: { ...ALL_OFF, url: true, version: true, authors: true, filename: true, enabled: true },
        customTemplate: '',
        customExtension: 'txt',
      },
    );
    expect(text).toBe(
      'Alpha (https://modrinth.com/mod/alpha123) (alpha.jar) [enabled]',
    );
  });

  it('keeps one plain-text line per mod even when metadata contains breaks', () => {
    const text = generateModList(
      [entry({ name: 'Alpha\nBeta', authors: ['Alice\r\nBob'] })],
      {
        format: 'text',
        options: { ...ALL_OFF, authors: true },
        customTemplate: '',
        customExtension: 'txt',
      },
    );
    expect(text.split('\n')).toHaveLength(1);
    expect(text).toContain('Alpha Beta');
    expect(text).toContain('by Alice Bob');
  });
});

describe('JSON generation', () => {
  it('produces a valid array with the full object structure and Unicode', () => {
    const json = generateModList(
      [
        entry({
          name: 'Мод «Пример»',
          authors: ['Иван', 'Alice'],
          enabled: false,
          version: '',
          url: '',
          modId: '',
        }),
      ],
      {
        format: 'json',
        options: ALL_OFF,
        customTemplate: '',
        customExtension: 'json',
      },
    );
    const parsed = JSON.parse(json);
    expect(Array.isArray(parsed)).toBe(true);
    expect(parsed[0]).toEqual({
      name: 'Мод «Пример»',
      mod_id: '',
      version: '',
      authors: ['Иван', 'Alice'],
      url: '',
      filename: 'alpha.jar',
      enabled: false,
    });
    expect(json.endsWith('\n')).toBe(true);
  });

  it('keeps an empty JSON export valid', () => {
    const json = generateModList([], {
      format: 'json',
      options: ALL_OFF,
      customTemplate: '',
      customExtension: 'json',
    });
    expect(json).toBe('[]\n');
    expect(JSON.parse(json)).toEqual([]);
  });
});

describe('CSV generation', () => {
  it('quotes commas, quotes and line breaks and uses CRLF records', () => {
    const csv = generateModList(
      [
        entry({
          name: 'Mod, "quoted"\nline',
          version: '1.0',
          authors: ['A, B'],
          filename: 'mod.jar',
        }),
      ],
      {
        format: 'csv',
        options: { version: true, authors: true, url: true, filename: true, enabled: true },
        customTemplate: '',
        customExtension: 'csv',
      },
    );
    const records = csv.split('\r\n');
    expect(records).toHaveLength(2);
    expect(records[0]).toBe('Name,URL,Version,Authors,File name,Enabled');
    expect(records[1]).toBe(
      '"Mod, ""quoted""\nline",https://modrinth.com/mod/alpha123,1.0,"A, B",mod.jar,true',
    );
  });

  it('always includes a header, even when no optional columns are selected', () => {
    const csv = generateModList([entry()], {
      format: 'csv',
      options: ALL_OFF,
      customTemplate: '',
      customExtension: 'csv',
    });
    expect(csv).toBe('Name\r\nAlpha');
  });
});

describe('custom templates', () => {
  it('substitutes every variable and keeps arbitrary text', () => {
    const text = generateCustomModList(
      [entry({ enabled: false })],
      '{name} | {mod_id} | {version} | {authors} | {url} | {filename} | {enabled}',
      'none',
    );
    expect(text).toBe(
      'Alpha | alpha_mod | 1.0.0 | Alice, Bob | https://modrinth.com/mod/alpha123 | alpha.jar | false',
    );
  });

  it('leaves unknown variables untouched and fills missing values with empty strings', () => {
    const text = generateCustomModList(
      [entry({ version: '', authors: [], url: '', modId: '' })],
      '{name}|{unknown}|{version}|{authors}|{url}|{mod_id}',
      'none',
    );
    expect(text).toBe('Alpha|{unknown}||||');
  });

  it('escapes inserted values according to the chosen extension', () => {
    const html = generateCustomModList(
      [entry({ name: '<script>', authors: ['A & B'] })],
      '<li><a href="{url}">{name}</a> — {authors}</li>',
      'html',
    );
    expect(html).toBe(
      '<li><a href="https://modrinth.com/mod/alpha123">&lt;script&gt;</a> — A &amp; B</li>',
    );
    const markdown = generateCustomModList(
      [entry({ name: '[Alpha]' })],
      '- [{name}]({url})',
      'markdown',
    );
    expect(markdown).toContain('\\[Alpha\\]');
    expect(templateEscapeForExtension('.HTML')).toBe('html');
    expect(templateEscapeForExtension('md')).toBe('markdown');
    expect(templateEscapeForExtension('json')).toBe('json');
    expect(templateEscapeForExtension('csv')).toBe('csv');
    expect(templateEscapeForExtension('txt')).toBe('none');
  });

  it('escapes JSON string contents and CSV cells for matching extensions', () => {
    const json = generateCustomModList(
      [entry({ name: 'A "quote"\nline' })],
      '{"name":"{name}"}',
      'json',
    );
    expect(json).toBe('{"name":"A \\"quote\\"\\nline"}');

    const csv = generateCustomModList(
      [entry({ name: 'A, B' })],
      '{name},{version}',
      'csv',
    );
    expect(csv).toBe('"A, B",1.0.0');
  });

  it('does not re-substitute tokens that appear inside mod data', () => {
    const text = generateCustomModList(
      [entry({ name: '{url}' })],
      '{name}|{url}',
      'none',
    );
    expect(text).toBe('{url}|https://modrinth.com/mod/alpha123');
  });

  it('provides a template example per built-in format', () => {
    expect(templateForFormat('html', 'custom')).toContain('<li>');
    expect(templateForFormat('markdown', 'custom')).toContain('[{name}]');
    expect(templateForFormat('json', 'custom')).toContain('"name"');
    expect(templateForFormat('custom', 'my {name}')).toBe('my {name}');
  });

  it('renders an empty result for an empty template or empty selection', () => {
    expect(generateCustomModList([entry()], '', 'none')).toBe('');
    expect(generateCustomModList([], '{name}', 'none')).toBe('');
  });
});

describe('preview regeneration and format metadata', () => {
  const settings = {
    format: 'text' as const,
    options: { ...ALL_OFF },
    customTemplate: '',
    customExtension: 'txt',
  };

  it('updates when an optional field is toggled', () => {
    const entries = [entry()];
    const withoutUrl = generateModList(entries, settings);
    const withUrl = generateModList(entries, {
      ...settings,
      options: { ...settings.options, url: true },
    });
    expect(withoutUrl).toBe('Alpha');
    expect(withUrl).toContain('https://modrinth.com/mod/alpha123');
    expect(withoutUrl).not.toBe(withUrl);
  });

  it('updates when the custom template changes', () => {
    const entries = [entry()];
    const first = generateModList(entries, {
      ...settings,
      format: 'custom',
      customTemplate: '{name}',
    });
    const second = generateModList(entries, {
      ...settings,
      format: 'custom',
      customTemplate: '{version} — {name}',
    });
    expect(first).toBe('Alpha');
    expect(second).toBe('1.0.0 — Alpha');
  });

  it('chooses the preview kind from the format and custom extension', () => {
    expect(previewKind('html', 'txt')).toBe('html');
    expect(previewKind('markdown', 'txt')).toBe('markdown');
    expect(previewKind('csv', 'txt')).toBe('text');
    expect(previewKind('custom', '.htm')).toBe('html');
    expect(previewKind('custom', 'md')).toBe('markdown');
    expect(previewKind('custom', 'xml')).toBe('text');
    expect(exportExtension('custom', '')).toBe('txt');
    expect(exportExtension('markdown', '')).toBe('md');
  });
});

describe('copying the complete result', () => {
  it('passes the whole generated document to the clipboard', async () => {
    const content = generateModList([entry()], {
      format: 'text',
      options: { ...ALL_OFF, url: true },
      customTemplate: '',
      customExtension: 'txt',
    });
    const writeText = vi.fn(async () => {});
    await copyModListText(content, { writeText });
    expect(writeText).toHaveBeenCalledTimes(1);
    expect(writeText).toHaveBeenCalledWith(content);
  });

  it('surfaces clipboard failures', async () => {
    await expect(
      copyModListText('text', {
        writeText: async () => {
          throw new Error('denied');
        },
      }),
    ).rejects.toThrow('denied');
  });

  it('never falls back to navigator.clipboard', async () => {
    await expect(
      copyModListText('text', undefined as never),
    ).rejects.toThrow('Native clipboard writer is not available');
  });
});

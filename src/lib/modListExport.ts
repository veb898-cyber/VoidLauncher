/**
 * Mod list export: pure data preparation and format generation.
 *
 * Everything in this module is independent of React and Tauri so it can be
 * unit-tested directly. The dialog only supplies the already-loaded metadata
 * from the mods tab (no network requests, no JAR rescans) and renders the
 * generated string.
 */

import { isKnownModVersion } from './modDisplay';

export const MOD_LIST_FORMATS = [
  'html',
  'markdown',
  'text',
  'json',
  'csv',
  'custom',
] as const;

export type ModListFormat = (typeof MOD_LIST_FORMATS)[number];

export const MOD_LIST_VARIABLES = [
  'name',
  'mod_id',
  'version',
  'authors',
  'url',
  'filename',
  'enabled',
] as const;

export type ModListVariable = (typeof MOD_LIST_VARIABLES)[number];

/** Substitution tokens shown by the template editor. */
export const MOD_LIST_VARIABLE_TOKENS: Record<ModListVariable, string> = {
  name: '{name}',
  mod_id: '{mod_id}',
  version: '{version}',
  authors: '{authors}',
  url: '{url}',
  filename: '{filename}',
  enabled: '{enabled}',
};

export interface ModListExportOptions {
  version: boolean;
  authors: boolean;
  url: boolean;
  filename: boolean;
  enabled: boolean;
}

/** Additional fields are off by default, matching Prism Launcher. */
export const DEFAULT_MOD_LIST_EXPORT_OPTIONS: ModListExportOptions = {
  version: false,
  authors: false,
  url: false,
  filename: false,
  enabled: false,
};

export const DEFAULT_CUSTOM_TEMPLATE = '{name} | {version} | {authors} | {url}';

/**
 * Prism-style examples shown in the Template box for the built-in formats.
 * Editing one switches the dialog to Custom, exactly like Prism's template
 * field; the examples are never used by the built-in generators.
 */
export const MOD_LIST_TEMPLATE_EXAMPLES: Record<
  Exclude<ModListFormat, 'custom'>,
  string
> = {
  html: '<li><a href="{url}">{name}</a> [{version}] by {authors}</li>',
  markdown: '[{name}]({url}) [{version}] by {authors}',
  text: '{name} ({url}) [{version}] by {authors}',
  json: '{"name":"{name}","url":"{url}","version":"{version}","authors":"{authors}"},',
  csv: '{name},{url},{version},"{authors}"',
};

export function templateForFormat(
  format: ModListFormat,
  customTemplate: string,
): string {
  return format === 'custom'
    ? customTemplate
    : MOD_LIST_TEMPLATE_EXAMPLES[format];
}

/**
 * The subset of `ContentItem` the export needs. Keeping the contract here
 * avoids coupling the pure generator to the mods page component.
 */
export interface ModListSourceItem {
  filename: string;
  name: string;
  version?: string | null;
  version_known?: boolean;
  provider?: string | null;
  enabled: boolean;
  project_id?: string | null;
  mod_id?: string | null;
  authors?: string[] | null;
  homepage?: string | null;
}

export interface ModListEntry {
  name: string;
  modId: string;
  url: string;
  version: string;
  authors: string[];
  filename: string;
  enabled: boolean;
}

export interface ModListGenerateOptions {
  format: ModListFormat;
  options: ModListExportOptions;
  customTemplate: string;
  customExtension: string;
  /** Used only for the HTML document title. */
  instanceName?: string;
}

const NAME_COLLATOR = new Intl.Collator(undefined, {
  sensitivity: 'base',
  numeric: true,
});

function cleanText(value: string | null | undefined): string {
  return typeof value === 'string' ? value.trim() : '';
}

function fallbackName(filename: string): string {
  return filename.replace(/\.jar(?:\.disabled)?$/i, '') || filename;
}

function exportVersion(item: ModListSourceItem): string {
  if (item.version_known === false) return '';
  const version = cleanText(item.version);
  return isKnownModVersion(version) ? version : '';
}

function exportAuthors(item: ModListSourceItem): string[] {
  if (!Array.isArray(item.authors)) return [];
  const seen = new Set<string>();
  const authors: string[] = [];
  for (const value of item.authors) {
    if (typeof value !== 'string') continue;
    const name = value.trim();
    if (!name) continue;
    const key = name.toLowerCase();
    if (seen.has(key)) continue;
    seen.add(key);
    authors.push(name);
  }
  return authors;
}

function normalizeHomepage(value: string | null | undefined): string {
  const url = cleanText(value);
  if (!url || /\s/.test(url)) return '';
  const match = /^https?:\/\/(.*)$/i.exec(url);
  if (!match || match[1].replace(/^\/+/, '').length === 0) return '';
  return url;
}

/**
 * Canonical project page derived from local provenance. The project id comes
 * from the launcher sidecar or packwiz metadata; a jar-internal slug is never
 * used because it is not guaranteed to be the Modrinth/CurseForge slug.
 */
function projectUrl(
  provider: string | null | undefined,
  projectId: string | null | undefined,
): string {
  const id = cleanText(projectId);
  if (!/^[A-Za-z0-9_-]{1,64}$/.test(id)) return '';
  const source = cleanText(provider).toLowerCase();
  if (source === 'modrinth') return `https://modrinth.com/mod/${id}`;
  if (source === 'curseforge') {
    return `https://www.curseforge.com/minecraft/mc-mods/${id}`;
  }
  return '';
}

function exportUrl(item: ModListSourceItem): string {
  return projectUrl(item.provider, item.project_id) || normalizeHomepage(item.homepage);
}

function exportModId(item: ModListSourceItem): string {
  // The backend reads this from the JAR descriptor. A missing id is exported
  // as an empty value rather than guessed from the project slug: a verified
  // slug is a provider project id, and an unverified one may still come from
  // packwiz metadata, not from the JAR.
  return cleanText(item.mod_id);
}

function toEntry(item: ModListSourceItem): ModListEntry {
  return {
    name: cleanText(item.name) || fallbackName(item.filename),
    modId: exportModId(item),
    url: exportUrl(item),
    version: exportVersion(item),
    authors: exportAuthors(item),
    filename: item.filename,
    enabled: item.enabled,
  };
}

/**
 * Selection semantics are intentionally absent: the export always contains
 * every installed mod of the instance, including disabled entries. The page's
 * search box is a view filter only and is never passed here.
 */
export function buildModListEntries(
  items: readonly ModListSourceItem[],
): ModListEntry[] {
  return items
    .map(toEntry)
    .sort(
      (a, b) =>
        NAME_COLLATOR.compare(a.name, b.name) ||
        NAME_COLLATOR.compare(a.filename, b.filename),
    );
}

/** Extension (without the dot) for the selected format. */
export function normalizeExportExtension(raw: string | null | undefined): string {
  return String(raw ?? '')
    .trim()
    .replace(/^\.+/, '')
    .toLowerCase()
    .replace(/[^a-z0-9]/g, '')
    .slice(0, 10);
}

export function exportExtension(
  format: ModListFormat,
  customExtension: string,
): string {
  switch (format) {
    case 'html':
      return 'html';
    case 'markdown':
      return 'md';
    case 'text':
      return 'txt';
    case 'json':
      return 'json';
    case 'csv':
      return 'csv';
    case 'custom':
      return normalizeExportExtension(customExtension) || 'txt';
  }
}

export type ModListPreviewKind = 'html' | 'markdown' | 'text';

/** Custom templates get a rendered preview only for a document extension. */
export function previewKind(
  format: ModListFormat,
  customExtension: string,
): ModListPreviewKind {
  if (format === 'html') return 'html';
  if (format === 'markdown') return 'markdown';
  if (format === 'custom') {
    const extension = normalizeExportExtension(customExtension);
    if (extension === 'html' || extension === 'htm') return 'html';
    if (extension === 'md' || extension === 'markdown') return 'markdown';
  }
  return 'text';
}

export type CustomTemplateEscape = 'none' | 'html' | 'markdown' | 'json' | 'csv';

/**
 * The custom template is raw user text: HTML/Markdown markup is allowed.
 * Only the substituted values are escaped, and the file extension decides
 * the context (`.html` -> HTML, `.md` -> Markdown, `.json` -> JSON string
 * contents, `.csv` -> CSV cells, everything else -> plain).
 */
export function templateEscapeForExtension(
  customExtension: string,
): CustomTemplateEscape {
  const extension = normalizeExportExtension(customExtension);
  if (extension === 'html' || extension === 'htm') return 'html';
  if (extension === 'md' || extension === 'markdown') return 'markdown';
  if (extension === 'json') return 'json';
  if (extension === 'csv') return 'csv';
  return 'none';
}

export function escapeHtml(value: string): string {
  return value
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

const MARKDOWN_SPECIAL = /[\\`*_{}\[\]<>()#+\-!|]/g;

/** Escape a value inserted into Markdown prose or a link label. */
export function escapeMarkdown(value: string): string {
  return value.replace(MARKDOWN_SPECIAL, (character) => `\\${character}`);
}

/** Keep a URL usable inside `[label](url)` without letting it break the link. */
export function escapeMarkdownUrl(url: string): string {
  return url
    .replace(/[\r\n]/g, '')
    .replace(/\\/g, '%5C')
    .replace(/ /g, '%20')
    .replace(/\(/g, '%28')
    .replace(/\)/g, '%29')
    .replace(/</g, '%3C')
    .replace(/>/g, '%3E')
    .replace(/"/g, '%22');
}

function statusLabel(enabled: boolean): string {
  return enabled ? '[enabled]' : '[disabled]';
}

/** Plain text promises one mod per line; metadata line breaks become spaces. */
function singleLine(value: string): string {
  return value.replace(/[\r\n]+/g, ' ').trim();
}

function htmlEntry(entry: ModListEntry, options: ModListExportOptions): string {
  const name = escapeHtml(entry.name);
  const linked =
    options.url && entry.url
      ? `<a href="${escapeHtml(entry.url)}">${name}</a>`
      : name;
  const parts = [`<li>${linked}`];
  if (options.version && entry.version) {
    parts.push(` [${escapeHtml(entry.version)}]`);
  }
  if (options.authors && entry.authors.length > 0) {
    parts.push(` by ${escapeHtml(entry.authors.join(', '))}`);
  }
  if (options.filename) {
    parts.push(` (${escapeHtml(entry.filename)})`);
  }
  if (options.enabled) {
    parts.push(` ${statusLabel(entry.enabled)}`);
  }
  parts.push('</li>');
  return parts.join('');
}

function markdownEntry(
  entry: ModListEntry,
  options: ModListExportOptions,
): string {
  const name = escapeMarkdown(entry.name);
  const linked =
    options.url && entry.url
      ? `[${name}](${escapeMarkdownUrl(entry.url)})`
      : name;
  const parts = [`- ${linked}`];
  if (options.version && entry.version) {
    parts.push(` [${escapeMarkdown(entry.version)}]`);
  }
  if (options.authors && entry.authors.length > 0) {
    parts.push(` by ${escapeMarkdown(entry.authors.join(', '))}`);
  }
  if (options.filename) {
    parts.push(` (${escapeMarkdown(entry.filename)})`);
  }
  if (options.enabled) {
    parts.push(` ${statusLabel(entry.enabled)}`);
  }
  return parts.join('');
}

function textEntry(entry: ModListEntry, options: ModListExportOptions): string {
  const parts = [singleLine(entry.name)];
  if (options.url && entry.url) {
    parts.push(` (${singleLine(entry.url)})`);
  }
  if (options.version && entry.version) {
    parts.push(` [${singleLine(entry.version)}]`);
  }
  if (options.authors && entry.authors.length > 0) {
    parts.push(` by ${singleLine(entry.authors.join(', '))}`);
  }
  if (options.filename) {
    parts.push(` (${singleLine(entry.filename)})`);
  }
  if (options.enabled) {
    parts.push(` ${statusLabel(entry.enabled)}`);
  }
  return parts.join('');
}

function jsonEntry(entry: ModListEntry) {
  return {
    name: entry.name,
    mod_id: entry.modId,
    version: entry.version,
    authors: entry.authors,
    url: entry.url,
    filename: entry.filename,
    enabled: entry.enabled,
  };
}

function escapeCsvCell(value: string): string {
  if (/[",\r\n]/.test(value) || /^\s|\s$/.test(value)) {
    return `"${value.replace(/"/g, '""')}"`;
  }
  return value;
}

/** Escape a value for use inside an existing JSON string literal. */
function escapeJsonValue(value: string): string {
  return JSON.stringify(value).slice(1, -1);
}

function csvContent(
  entries: readonly ModListEntry[],
  options: ModListExportOptions,
): string {
  const header = ['Name'];
  if (options.url) header.push('URL');
  if (options.version) header.push('Version');
  if (options.authors) header.push('Authors');
  if (options.filename) header.push('File name');
  if (options.enabled) header.push('Enabled');

  const rows = entries.map((entry) => {
    const cells = [entry.name];
    if (options.url) cells.push(entry.url);
    if (options.version) cells.push(entry.version);
    if (options.authors) cells.push(entry.authors.join(', '));
    if (options.filename) cells.push(entry.filename);
    if (options.enabled) cells.push(entry.enabled ? 'true' : 'false');
    return cells.map(escapeCsvCell).join(',');
  });

  return [header.map(escapeCsvCell).join(','), ...rows].join('\r\n');
}

function customValue(entry: ModListEntry, variable: ModListVariable): string {
  switch (variable) {
    case 'name':
      return entry.name;
    case 'mod_id':
      return entry.modId;
    case 'version':
      return entry.version;
    case 'authors':
      return entry.authors.join(', ');
    case 'url':
      return entry.url;
    case 'filename':
      return entry.filename;
    case 'enabled':
      return entry.enabled ? 'true' : 'false';
  }
}

function escapeCustomValue(
  variable: ModListVariable,
  value: string,
  escape: CustomTemplateEscape,
): string {
  if (escape === 'html') return escapeHtml(value);
  if (escape === 'markdown') {
    return variable === 'url' ? escapeMarkdownUrl(value) : escapeMarkdown(value);
  }
  if (escape === 'json') return escapeJsonValue(value);
  if (escape === 'csv') return escapeCsvCell(value);
  return value;
}

/**
 * Replace known `{variable}` tokens only. Unknown tokens are intentionally
 * left untouched so a typo is visible in the preview instead of silently
 * producing an empty string.
 */
export function generateCustomModList(
  entries: readonly ModListEntry[],
  template: string,
  escape: CustomTemplateEscape,
): string {
  if (entries.length === 0 || template.length === 0) return '';
  return entries
    .map((entry) =>
      template.replace(
        /\{(name|mod_id|version|authors|url|filename|enabled)\}/g,
        (_match, variable: ModListVariable) =>
          escapeCustomValue(variable, customValue(entry, variable), escape),
      ),
    )
    .join('\n');
}

/**
 * Generate the text for one format. JSON always contains the complete object
 * set; the optional-field switches apply to the human-readable formats.
 */
export function generateModList(
  entries: readonly ModListEntry[],
  settings: ModListGenerateOptions,
): string {
  const { format, options } = settings;
  if (entries.length === 0) {
    // An empty JSON export is still a valid array; the dialog shows its empty
    // state and disables saving, but the generator keeps the format contract.
    return format === 'json' ? '[]\n' : '';
  }

  switch (format) {
    case 'html': {
      const titleName = cleanText(settings.instanceName);
      const title = escapeHtml(
        titleName ? `${titleName} — mod list` : 'Mod list',
      );
      const lines = entries.map(
        (entry) => `\t${htmlEntry(entry, options)}`,
      );
      return [
        '<!DOCTYPE html>',
        '<html>',
        '<head>',
        '  <meta charset="utf-8">',
        `  <title>${title}</title>`,
        '</head>',
        '<body>',
        '<ul>',
        ...lines,
        '</ul>',
        '</body>',
        '</html>',
      ].join('\n');
    }
    case 'markdown':
      return entries.map((entry) => markdownEntry(entry, options)).join('\n');
    case 'text':
      return entries.map((entry) => textEntry(entry, options)).join('\n');
    case 'json':
      return `${JSON.stringify(entries.map(jsonEntry), null, 2)}\n`;
    case 'csv':
      return csvContent(entries, options);
    case 'custom':
      return generateCustomModList(
        entries,
        settings.customTemplate,
        templateEscapeForExtension(settings.customExtension),
      );
  }
}

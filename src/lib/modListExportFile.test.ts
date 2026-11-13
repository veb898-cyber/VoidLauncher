import { describe, expect, it, vi } from 'vitest';
import {
  ensureExportExtension,
  insertTemplateVariable,
  sanitizeFileNamePart,
  saveModListExport,
  shouldAddUtf8Bom,
  suggestedExportFilename,
} from './modListExportFile';

describe('export file names', () => {
  it('builds a safe suggested name from the instance name', () => {
    expect(suggestedExportFilename('My Pack', 'html')).toBe('My Pack - mods.html');
    expect(suggestedExportFilename('Сборка', 'html')).toBe('Сборка - mods.html');
    expect(suggestedExportFilename('A/B:C*?', 'json')).toBe('A_B_C__ - mods.json');
    expect(suggestedExportFilename('   ', 'txt')).toBe('instance - mods.txt');
    expect(suggestedExportFilename('Pack', '')).toBe('Pack - mods.txt');
  });

  it('sanitizes Windows-invalid characters and trailing dots/spaces', () => {
    expect(sanitizeFileNamePart(' A/B:C*?"<>| ')).toBe('A_B_C______');
    expect(sanitizeFileNamePart('name...   ')).toBe('name');
    expect(sanitizeFileNamePart('')).toBe('instance');
  });

  it('appends the selected extension only when it is missing', () => {
    expect(ensureExportExtension('/tmp/list', 'html')).toBe('/tmp/list.html');
    expect(ensureExportExtension('/tmp/list.HTML', 'html')).toBe('/tmp/list.HTML');
    expect(ensureExportExtension('C:\\exports\\list.md', 'md')).toBe(
      'C:\\exports\\list.md',
    );
    expect(ensureExportExtension('/tmp/list.txt', 'md')).toBe('/tmp/list.txt.md');
  });

  it('adds a UTF-8 BOM only for CSV', () => {
    expect(shouldAddUtf8Bom('csv', '')).toBe(true);
    expect(shouldAddUtf8Bom('custom', 'csv')).toBe(true);
    expect(shouldAddUtf8Bom('html', '')).toBe(false);
    expect(shouldAddUtf8Bom('custom', 'txt')).toBe(false);
  });
});

describe('template variable insertion', () => {
  it('inserts at the cursor and preserves text on both sides', () => {
    expect(insertTemplateVariable('{name} — ', 9, 9, '{version}')).toEqual({
      value: '{name} — {version}',
      cursor: 18,
    });
  });

  it('replaces the current selection', () => {
    expect(insertTemplateVariable('a XX b', 2, 4, '{url}')).toEqual({
      value: 'a {url} b',
      cursor: 7,
    });
  });

  it('clamps out-of-range selections', () => {
    expect(insertTemplateVariable('abc', 99, 120, '{name}')).toEqual({
      value: 'abc{name}',
      cursor: 9,
    });
  });
});

describe('saving the export', () => {
  const request = {
    content: 'Alpha\r\nBeta',
    instanceName: 'My Pack',
    format: 'html' as const,
    customExtension: 'html',
  };

  it('saves with the format extension and reports the final path', async () => {
    const choosePath = vi.fn(async () => '/exports/my-list');
    const writeFile = vi.fn(async () => {});
    const result = await saveModListExport(request, { choosePath, writeFile });
    expect(choosePath).toHaveBeenCalledWith('My Pack - mods.html', 'html');
    expect(writeFile).toHaveBeenCalledWith(
      '/exports/my-list.html',
      request.content,
      false,
    );
    expect(result).toEqual({
      status: 'saved',
      path: '/exports/my-list.html',
      extension: 'html',
    });
  });

  it('uses the custom extension and enables the BOM for CSV', async () => {
    const choosePath = vi.fn(async () => '/exports/list');
    const writeFile = vi.fn(async () => {});
    const result = await saveModListExport(
      { ...request, format: 'custom', customExtension: 'csv' },
      { choosePath, writeFile },
    );
    expect(choosePath).toHaveBeenCalledWith('My Pack - mods.csv', 'csv');
    expect(writeFile).toHaveBeenCalledWith('/exports/list.csv', request.content, true);
    expect(result.status).toBe('saved');
  });

  it('treats a cancelled native dialog as a normal outcome', async () => {
    const choosePath = vi.fn(async () => null);
    const writeFile = vi.fn(async () => {});
    const result = await saveModListExport(request, { choosePath, writeFile });
    expect(result).toEqual({ status: 'cancelled' });
    expect(writeFile).not.toHaveBeenCalled();
  });

  it('propagates write errors to the dialog', async () => {
    const choosePath = vi.fn(async () => '/exports/list');
    const writeFile = vi.fn(async () => {
      throw new Error('disk full');
    });
    await expect(
      saveModListExport(request, { choosePath, writeFile }),
    ).rejects.toThrow('disk full');
  });
});

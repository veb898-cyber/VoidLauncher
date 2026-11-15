import { beforeEach, describe, expect, it } from 'vitest';
import {
  DEFAULT_MOD_LIST_EXPORT_SETTINGS,
  MOD_LIST_EXPORT_STORAGE_KEY,
  loadModListExportSettings,
  parseModListExportSettings,
  useModListExportStore,
} from './modListExportStore';

beforeEach(() => {
  localStorage.clear();
  useModListExportStore.setState({
    ...DEFAULT_MOD_LIST_EXPORT_SETTINGS,
    options: { ...DEFAULT_MOD_LIST_EXPORT_SETTINGS.options },
  });
});

describe('mod list export settings', () => {
  it('uses Prism-like defaults on a fresh install', () => {
    expect(loadModListExportSettings()).toEqual({
      format: 'html',
      options: {
        version: false,
        authors: false,
        url: false,
        filename: false,
        enabled: false,
      },
      customTemplate: '{name} | {version} | {authors} | {url}',
      customExtension: 'txt',
    });
  });

  it('restores every setting after the dialog is closed and reopened', () => {
    const store = useModListExportStore.getState();
    store.setFormat('csv');
    store.setOption('version', true);
    store.setOption('authors', true);
    store.setOption('enabled', true);
    store.setCustomTemplate('{name};{enabled}');
    store.setCustomExtension('.XML');

    const reopened = loadModListExportSettings();
    expect(reopened).toEqual({
      format: 'csv',
      options: {
        version: true,
        authors: true,
        url: false,
        filename: false,
        enabled: true,
      },
      customTemplate: '{name};{enabled}',
      customExtension: 'xml',
    });

    const persisted = JSON.parse(
      localStorage.getItem(MOD_LIST_EXPORT_STORAGE_KEY) ?? '{}',
    );
    expect(persisted.format).toBe('csv');
    expect(persisted.customExtension).toBe('xml');
  });

  it('does not persist the transient mod selection', () => {
    useModListExportStore.getState().setFormat('json');
    const persisted = localStorage.getItem(MOD_LIST_EXPORT_STORAGE_KEY) ?? '';
    expect(persisted).not.toContain('selected');
    expect(persisted).not.toContain('selectedFilenames');
  });

  it('normalizes an extension typed with a dot, spaces or uppercase', () => {
    useModListExportStore.getState().setCustomExtension(' .HTML ');
    expect(useModListExportStore.getState().customExtension).toBe('html');
    useModListExportStore.getState().setCustomExtension('a-b_c');
    expect(useModListExportStore.getState().customExtension).toBe('abc');
  });

  it('falls back safely when the stored payload is partial or invalid', () => {
    expect(parseModListExportSettings(null)).toEqual(
      DEFAULT_MOD_LIST_EXPORT_SETTINGS,
    );
    expect(
      parseModListExportSettings({
        format: 'exe',
        options: { version: 'yes', url: true },
        customTemplate: 42,
        customExtension: '???',
      }),
    ).toEqual({
      format: 'html',
      options: {
        version: false,
        authors: false,
        url: true,
        filename: false,
        enabled: false,
      },
      customTemplate: '{name} | {version} | {authors} | {url}',
      customExtension: 'txt',
    });

    localStorage.setItem(MOD_LIST_EXPORT_STORAGE_KEY, '{not json');
    expect(loadModListExportSettings()).toEqual(
      DEFAULT_MOD_LIST_EXPORT_SETTINGS,
    );
  });
});

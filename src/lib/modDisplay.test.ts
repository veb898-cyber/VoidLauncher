import { describe, expect, it } from 'vitest';
import {
  isKnownModVersion,
  modLoadersLabel,
  modVersionLabel,
  normalizeModLoaders,
} from './modDisplay';

describe('mod version display', () => {
  it('shows an explicit Unknown for an empty or missing version', () => {
    expect(modVersionLabel('', true, 'Unknown')).toBe('Unknown');
    expect(modVersionLabel(undefined, undefined, 'Unknown')).toBe('Unknown');
  });

  it('shows Unknown for known placeholder values', () => {
    for (const placeholder of ['${version}', '${file.jarVersion}', 'Unknown', 'NONE', 'n/a', '-']) {
      expect(modVersionLabel(placeholder, true, 'Unknown')).toBe('Unknown');
      expect(isKnownModVersion(placeholder)).toBe(false);
    }
  });

  it('uses the backend version_known flag even when a stale string exists', () => {
    expect(modVersionLabel('2.4.1', false, 'Unknown')).toBe('Unknown');
  });

  it('keeps a real version and never appends the source or MC version', () => {
    expect(modVersionLabel('2.4.1', true, 'Unknown')).toBe('2.4.1');
    expect(modVersionLabel('1.21.1-1.4.8', true, 'Unknown')).toBe('1.21.1-1.4.8');
  });

  it('keeps old metadata without version_known readable', () => {
    expect(modVersionLabel('1.0.0', undefined, 'Unknown')).toBe('1.0.0');
  });
});

describe('mod loader display', () => {
  it('shows every supported loader of a multi-loader mod', () => {
    expect(modLoadersLabel(['Fabric', 'NeoForge'])).toBe('Fabric / NeoForge');
    expect(modLoadersLabel(['Fabric', 'Forge', 'NeoForge'])).toBe(
      'Fabric / Forge / NeoForge',
    );
  });

  it('deduplicates loaders without changing their order', () => {
    expect(modLoadersLabel(['NeoForge', 'Fabric', 'NeoForge'])).toBe('NeoForge / Fabric');
  });

  it('never turns an unknown loader into Fabric, Forge or NeoForge', () => {
    expect(normalizeModLoaders(['Quilt'])).toEqual([]);
    expect(modLoadersLabel(['Quilt', 'Fabric'])).toBe('Fabric');
  });

  it('shows nothing for old metadata without loaders', () => {
    expect(modLoadersLabel(undefined)).toBe('');
    expect(modLoadersLabel([])).toBe('');
  });
});

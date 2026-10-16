import { afterEach, describe, expect, it } from 'vitest';
import { formatDownloads, formatSize } from './format';
import { useLanguageStore } from '../stores/languageStore';

const setLanguage = (language: 'en' | 'ru') =>
  useLanguageStore.setState({ language });

afterEach(() => setLanguage('en'));

describe('formatSize', () => {
  it('formats sizes with English units', () => {
    expect(formatSize(1024)).toBe('1.0 KB');
    expect(formatSize(2 * 1_048_576 + 512 * 1024)).toBe('2.5 MB');
    expect(formatSize(1_073_741_824)).toBe('1.00 GB');
  });

  it('formats sizes with Russian units', () => {
    setLanguage('ru');
    expect(formatSize(1024)).toBe('1.0 КБ');
    expect(formatSize(1_048_576)).toBe('1.0 МБ');
    expect(formatSize(1_073_741_824)).toBe('1.00 ГБ');
  });
});

describe('formatDownloads', () => {
  it('uses K/M suffixes in English', () => {
    expect(formatDownloads(999)).toBe('999');
    expect(formatDownloads(5_300)).toBe('5.3K');
    expect(formatDownloads(1_200_000)).toBe('1.2M');
  });

  it('uses Russian suffixes', () => {
    setLanguage('ru');
    expect(formatDownloads(5_300)).toBe('5.3к');
    expect(formatDownloads(1_200_000)).toBe('1.2млн');
  });
});

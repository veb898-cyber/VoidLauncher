import { afterEach, describe, expect, it } from 'vitest';
import { formatPlayTime, formatRelativeTime } from './index';
import { useLanguageStore } from '../../stores/languageStore';

const setLanguage = (language: 'en' | 'ru') =>
  useLanguageStore.setState({ language });

afterEach(() => setLanguage('en'));

describe('formatPlayTime', () => {
  it('formats hours and minutes in English', () => {
    expect(formatPlayTime(3661)).toBe('1 hour 1 minute');
    expect(formatPlayTime(7200)).toBe('2 hours');
    expect(formatPlayTime(59)).toBe('0 minutes');
  });

  it('uses Russian plural cases', () => {
    setLanguage('ru');
    expect(formatPlayTime(3661)).toBe('1 час 1 минуту');
    expect(formatPlayTime(2 * 3600 + 3 * 60)).toBe('2 часа 3 минуты');
    expect(formatPlayTime(5 * 3600)).toBe('5 часов');
    expect(formatPlayTime(11 * 3600 + 11 * 60)).toBe('11 часов 11 минут');
  });
});

describe('formatRelativeTime', () => {
  it('formats recent timestamps in English', () => {
    const now = Date.now();
    expect(formatRelativeTime(new Date(now - 5_000).toISOString())).toBe('just now');
    expect(formatRelativeTime(new Date(now - 3 * 60_000).toISOString())).toBe(
      '3 minutes ago',
    );
    expect(formatRelativeTime(new Date(now - 2 * 3_600_000).toISOString())).toBe(
      '2 hours ago',
    );
    expect(formatRelativeTime(new Date(now - 3 * 86_400_000).toISOString())).toBe(
      '3 days ago',
    );
  });

  it('formats Russian plural forms', () => {
    setLanguage('ru');
    const now = Date.now();
    expect(formatRelativeTime(new Date(now - 2 * 60_000).toISOString())).toBe(
      '2 минуты назад',
    );
    expect(formatRelativeTime(new Date(now - 5 * 60_000).toISOString())).toBe(
      '5 минут назад',
    );
    expect(formatRelativeTime(new Date(now - 11 * 3_600_000).toISOString())).toBe(
      '11 часов назад',
    );
  });
});

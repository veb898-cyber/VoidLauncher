/**
 * Lightweight i18n. English is the default; Russian is fully translated
 * and selectable in Settings → Appearance → Language.
 *
 * `en.ts` is the source of truth for every user-facing string.
 * `ru.ts` mirrors it exactly with full Russian translations.
 *
 * Usage (in React components):
 *   import { useT } from '../lib/i18n';
 *   const t = useT();
 *   <h1>{t('instance_editor.gc_label')}</h1>
 *   <p>{t('instance_editor.memory_max_info', { maxGb: '6.0', systemRamGb: '16.0' })}</p>
 *
 * The `useT()` hook subscribes to the language store so switching
 * language re-renders the entire tree. The bare `t()` function reads
 * directly from the store and is safe for use in event handlers,
 * callbacks, and non-React code.
 */
import { en, type MessageKey } from './en';
import { ru } from './ru';
import { useLanguageStore } from '../../stores/languageStore';

export type Language = 'en' | 'ru';
export type { MessageKey };

const dictionaries: Record<Language, Record<string, string>> = { en, ru };

export function getLanguage(): Language {
  return useLanguageStore.getState().language;
}

/**
 * Translate a key. Falls back to English if the key is missing in the
 * current locale, then to the raw key as a last resort.
 * Variables in the form `{name}` are interpolated from `vars`.
 */
export function t(key: MessageKey, vars?: Record<string, string | number>): string {
  const lang = useLanguageStore.getState().language;
  const dict = dictionaries[lang] ?? en;
  let str = dict[key] ?? en[key] ?? key;
  if (vars) {
    for (const [name, val] of Object.entries(vars)) {
      str = str.replace(new RegExp(`\\{${name}\\}`, 'g'), String(val));
    }
  }
  return str;
}

/**
 * Format seconds as a human-readable, locale-aware play time string.
 * Examples (en): `formatPlayTime(3661)` → `"1 hour 1 minute"`
 * Examples (ru): `formatPlayTime(3661)` → `"1 час 1 минуту"`
 */
export function formatPlayTime(seconds: number): string {
  const lang = useLanguageStore.getState().language;
  const hours = Math.floor(seconds / 3600);
  const mins = Math.floor((seconds % 3600) / 60);

  if (lang === 'ru') {
    const pluralH = (n: number) => {
      const m10 = n % 10, m100 = n % 100;
      if (m10 === 1 && m100 !== 11) return 'час';
      if (m10 >= 2 && m10 <= 4 && !(m100 >= 12 && m100 <= 14)) return 'часа';
      return 'часов';
    };
    const pluralM = (n: number) => {
      const m10 = n % 10, m100 = n % 100;
      if (m10 === 1 && m100 !== 11) return 'минуту';
      if (m10 >= 2 && m10 <= 4 && !(m100 >= 12 && m100 <= 14)) return 'минуты';
      return 'минут';
    };
    if (hours > 0 && mins > 0) return `${hours} ${pluralH(hours)} ${mins} ${pluralM(mins)}`;
    if (hours > 0) return `${hours} ${pluralH(hours)}`;
    return `${mins} ${pluralM(mins)}`;
  }

  // English
  if (hours > 0 && mins > 0) return `${hours} ${hours === 1 ? 'hour' : 'hours'} ${mins} ${mins === 1 ? 'minute' : 'minutes'}`;
  if (hours > 0) return `${hours} ${hours === 1 ? 'hour' : 'hours'}`;
  return `${mins} ${mins === 1 ? 'minute' : 'minutes'}`;
}

/**
 * Format a timestamp as a relative time string (e.g., "2 hours ago", "вчера", "3 дня назад").
 */
export function formatRelativeTime(isoString: string): string {
  const lang = useLanguageStore.getState().language;
  const date = new Date(isoString);
  const now = new Date();
  const diffMs = now.getTime() - date.getTime();
  const diffSec = Math.floor(diffMs / 1000);
  const diffMin = Math.floor(diffSec / 60);
  const diffHour = Math.floor(diffMin / 60);
  const diffDay = Math.floor(diffHour / 24);

  if (diffSec < 60) {
    return lang === 'ru' ? 'только что' : 'just now';
  }
  if (diffMin < 60) {
    return lang === 'ru'
      ? `${diffMin} ${pluralRu(diffMin, 'минута', 'минуты', 'минут')} назад`
      : `${diffMin} ${diffMin === 1 ? 'minute' : 'minutes'} ago`;
  }
  if (diffHour < 24) {
    return lang === 'ru'
      ? `${diffHour} ${pluralRu(diffHour, 'час', 'часа', 'часов')} назад`
      : `${diffHour} ${diffHour === 1 ? 'hour' : 'hours'} ago`;
  }
  if (diffDay < 7) {
    return lang === 'ru'
      ? `${diffDay} ${pluralRu(diffDay, 'день', 'дня', 'дней')} назад`
      : `${diffDay} ${diffDay === 1 ? 'day' : 'days'} ago`;
  }
  // Older than a week: show date
  return date.toLocaleDateString(lang === 'ru' ? 'ru-RU' : 'en-US', {
    day: 'numeric',
    month: 'short',
    year: diffDay < 365 ? undefined : 'numeric',
  });
}

function pluralRu(n: number, one: string, few: string, many: string): string {
  const m10 = n % 10, m100 = n % 100;
  if (m10 === 1 && m100 !== 11) return one;
  if (m10 >= 2 && m10 <= 4 && !(m100 >= 12 && m100 <= 14)) return few;
  return many;
}

/**
 * React hook: subscribes to the active language and returns a `t` function
 * that reads the *current* language at call time. The returned function
 * reference is stable across renders that don't change the language.
 */
export function useT(): (key: MessageKey, vars?: Record<string, string | number>) => string {
  const lang = useLanguageStore((s) => s.language);
  return (key, vars) => {
    const dict = dictionaries[lang] ?? en;
    let str = dict[key] ?? en[key] ?? key;
    if (vars) {
      for (const [name, val] of Object.entries(vars)) {
        str = str.replace(new RegExp(`\\{${name}\\}`, 'g'), String(val));
      }
    }
    return str;
  };
}

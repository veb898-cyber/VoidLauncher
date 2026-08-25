import { getLanguage } from './i18n';

const isRu = () => getLanguage() === 'ru';

/** File size with localized units: КБ/МБ/ГБ (ru), KB/MB/GB (en). */
export function formatSize(bytes: number): string {
  const ru = isRu();
  if (bytes >= 1_073_741_824) return `${(bytes / 1_073_741_824).toFixed(2)} ${ru ? 'ГБ' : 'GB'}`;
  if (bytes >= 1_048_576) return `${(bytes / 1_048_576).toFixed(1)} ${ru ? 'МБ' : 'MB'}`;
  return `${(bytes / 1024).toFixed(1)} ${ru ? 'КБ' : 'KB'}`;
}

/** Alias used by the modpacks page. */
export const formatBytes = formatSize;

/** Download counter: 1.2млн / 5.3к (ru), 1.2M / 5.3K (en). */
export function formatDownloads(n: number): string {
  const ru = isRu();
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}${ru ? 'млн' : 'M'}`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}${ru ? 'к' : 'K'}`;
  return String(n);
}
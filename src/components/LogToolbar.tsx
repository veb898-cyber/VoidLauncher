import type { ReactNode } from 'react';
import { Search, X } from 'lucide-react';
import { useT } from '../lib/i18n';
import { Segmented } from './ui/Segmented';

/** Severity buckets shown as filters. `info` also carries debug lines. */
export type LogLevelFilter = 'error' | 'warn' | 'info';

export const LOG_LEVEL_FILTERS: readonly LogLevelFilter[] = ['error', 'warn', 'info'];

/** Bucket a raw/store level into the filter the user sees. */
export function toLevelFilter(level: string): LogLevelFilter {
  if (level === 'error') return 'error';
  if (level === 'warn' || level === 'warning') return 'warn';
  return 'info';
}

export type LevelFilterState = Record<LogLevelFilter, boolean>;

export function allLevelsOn(): LevelFilterState {
  return { error: true, warn: true, info: true };
}

export interface LogToolbarProps {
  query: string;
  onQueryChange: (value: string) => void;
  levels: LevelFilterState;
  onToggleLevel: (level: LogLevelFilter) => void;
  /** Page-specific control placed after the filters (e.g. the run picker). */
  leading?: ReactNode;
  /** Page-specific actions (copy / clear / open folder) pinned to the right. */
  actions?: ReactNode;
}

/**
 * Shared search + severity filter row for both Terminal tabs. Severity labels
 * stay in English on purpose: they mirror Java/launcher level names that users
 * see inside the log text itself.
 */
export function LogToolbar({ query, onQueryChange, levels, onToggleLevel, leading, actions }: LogToolbarProps) {
  const t = useT();

  return (
    <div className="log-toolbar">
      <div className="log-search">
        <span className="log-search__icon"><Search size={14} /></span>
        <input
          className="input log-search__input"
          type="search"
          value={query}
          placeholder={t('terminal.search_placeholder')}
          aria-label={t('terminal.search_placeholder')}
          onChange={(e) => onQueryChange(e.target.value)}
        />
        {query && (
          <button
            className="log-search__clear"
            type="button"
            aria-label={t('terminal.search_clear')}
            title={t('terminal.search_clear')}
            onClick={() => onQueryChange('')}
          >
            <X size={13} />
          </button>
        )}
      </div>

      <Segmented
        options={LOG_LEVEL_FILTERS.map((level) => ({ value: level, label: level.toUpperCase() }))}
        ariaLabel={t('terminal.filter_levels')}
        pressed={levels}
        onToggle={onToggleLevel}
      />

      {leading}

      {actions && <div className="log-toolbar__spacer" />}
      {actions}
    </div>
  );
}

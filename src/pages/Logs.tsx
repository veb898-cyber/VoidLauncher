import { useEffect, useMemo, useRef, useState } from 'react';
import { useLogStore } from '../stores/logStore';
import { useT } from '../lib/i18n';
import { addToast } from '../components/ui/Toast';
import {
  LogToolbar, allLevelsOn, toLevelFilter,
  type LevelFilterState, type LogLevelFilter,
} from '../components/LogToolbar';

export function LauncherLogs() {
  const t = useT();
  const { logs, clearLogs } = useLogStore();
  const bottomRef = useRef<HTMLDivElement>(null);
  const [query, setQuery] = useState('');
  const [levels, setLevels] = useState<LevelFilterState>(allLevelsOn);

  // Launcher activity only — game output lives in the "Игровые логи" tab
  // of the same Terminal page.
  const launcherLogs = useMemo(
    () => logs.filter((l) => l.source !== 'minecraft' && l.source !== 'launch'),
    [logs],
  );

  const visibleLogs = useMemo(() => {
    const needle = query.trim().toLowerCase();
    return launcherLogs.filter((log) => {
      if (!levels[toLevelFilter(log.level)]) return false;
      if (!needle) return true;
      return log.message.toLowerCase().includes(needle)
        || log.source.toLowerCase().includes(needle)
        || log.level.includes(needle);
    });
  }, [launcherLogs, levels, query]);

  const toggleLevel = (level: LogLevelFilter) => {
    setLevels((prev) => ({ ...prev, [level]: !prev[level] }));
  };

  // Follow the tail only while new lines arrive — filtering or searching must
  // not yank the viewport back down.
  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [launcherLogs.length]);

  const hasHiddenLines = visibleLogs.length !== launcherLogs.length;

  return (
    <div style={{ display: 'flex', flexDirection: 'column', height: '100%', gap: 'var(--space-md)' }}>
      <LogToolbar
        query={query}
        onQueryChange={setQuery}
        levels={levels}
        onToggleLevel={toggleLevel}
        actions={
          <>
            <button className="btn btn--secondary btn--sm" onClick={async () => {
              const text = visibleLogs.map(l => `[${l.timestamp}] [${l.source}] [${l.level.toUpperCase()}] ${l.message}`).join('\n');
              try {
                await navigator.clipboard.writeText(text);
                addToast(t('common.copied'), 'success');
              } catch {
                addToast(t('common.copy_failed'), 'error');
              }
            }}>
              {t('common.copy_all')}
            </button>
            <button className="btn btn--secondary btn--sm" onClick={clearLogs}>
              {t('common.clear')}
            </button>
          </>
        }
      />

      <div className="log-container" style={{
        flex: 1,
        overflowY: 'auto',
        background: 'var(--bg-primary)',
        borderRadius: 'var(--radius-lg)',
        padding: 'var(--space-md)',
        fontFamily: 'var(--font-mono)',
        fontSize: 'var(--font-size-xs)',
        lineHeight: 1.6,
      }}>
        {visibleLogs.length === 0 ? (
          <div style={{ color: 'var(--text-tertiary)', textAlign: 'center', paddingTop: 'var(--space-2xl)' }}>
            {launcherLogs.length === 0 ? t('logs.empty') : t('terminal.no_matches')}
          </div>
        ) : (
          visibleLogs.map((log) => (
            <div key={log.id} className={`log-line ${
              log.level === 'error' ? 'log-line--error' :
              log.level === 'warn' ? 'log-line--warn' :
              log.level === 'debug' ? 'log-line--debug' : ''
            }`} style={{
              display: 'flex',
              gap: 'var(--space-sm)',
              padding: '1px 0',
              whiteSpace: 'pre-wrap',
              wordBreak: 'break-all',
            }}>
              <span style={{ color: 'var(--text-tertiary)', flexShrink: 0 }}>{log.timestamp}</span>
              <span style={{
                flexShrink: 0,
                color: log.level === 'error' ? 'var(--color-danger)' :
                       log.level === 'warn' ? 'var(--color-warning)' :
                       log.level === 'debug' ? 'var(--text-tertiary)' :
                       'var(--text-secondary)',
                minWidth: 40,
              }}>
                {log.level.toUpperCase()}
              </span>
              <span style={{ color: 'var(--text-tertiary)', flexShrink: 0, minWidth: 60 }}>[{log.source}]</span>
              <span style={{ color: log.level === 'error' ? 'var(--color-danger)' : 'var(--text-primary)' }}>
                {log.message}
              </span>
            </div>
          ))
        )}
        {hasHiddenLines && visibleLogs.length > 0 && (
          <div style={{ color: 'var(--text-tertiary)', textAlign: 'center', paddingTop: 'var(--space-sm)' }}>
            {t('terminal.hidden_count', { count: String(launcherLogs.length - visibleLogs.length) })}
          </div>
        )}
        <div ref={bottomRef} />
      </div>
    </div>
  );
}

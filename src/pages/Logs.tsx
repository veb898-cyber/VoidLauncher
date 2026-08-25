import { useEffect, useRef } from 'react';
import { useLogStore } from '../stores/logStore';
import { useT } from '../lib/i18n';

export function Logs() {
  const t = useT();
  const { logs, clearLogs } = useLogStore();
  const bottomRef = useRef<HTMLDivElement>(null);

  // Launcher activity only — game output lives in its own "Game Logs" page.
  const launcherLogs = logs.filter((l) => l.source !== 'minecraft' && l.source !== 'launch');

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [launcherLogs.length]);

  return (
    <div className="page animate-fade-in" style={{ display: 'flex', flexDirection: 'column', height: '100%' }}>
      <div className="page__header" style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', flexShrink: 0 }}>
        <div>
          <h1 className="page__title">{t('logs.title')}</h1>
          <p className="page__subtitle">{t('logs.subtitle')}</p>
        </div>
        <div style={{ display: 'flex', gap: 'var(--space-sm)', alignItems: 'center' }}>
          <button className="btn btn--ghost btn--sm" onClick={() => {
            const text = launcherLogs.map(l => `[${l.timestamp}] [${l.source}] [${l.level.toUpperCase()}] ${l.message}`).join('\n');
            navigator.clipboard.writeText(text);
          }}>
            {t('common.copy_all')}
          </button>
          <button className="btn btn--ghost btn--sm" onClick={clearLogs}>
            {t('common.clear')}
          </button>
        </div>
      </div>

      <div className="log-container" style={{
        flex: 1,
        overflowY: 'auto',
        background: 'var(--bg-primary)',
        borderRadius: 'var(--radius-lg)',
        padding: 'var(--space-md)',
        fontFamily: "'Cascadia Code', 'Fira Code', 'JetBrains Mono', monospace",
        fontSize: 'var(--font-size-xs)',
        lineHeight: 1.6,
      }}>
        {launcherLogs.length === 0 ? (
          <div style={{ color: 'var(--text-tertiary)', textAlign: 'center', paddingTop: 'var(--space-2xl)' }}>
            {t('logs.empty')}
          </div>
        ) : (
          launcherLogs.map((log) => (
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
        <div ref={bottomRef} />
      </div>
    </div>
  );
}

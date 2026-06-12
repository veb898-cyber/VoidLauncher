import { useEffect, useRef, useState, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { useT } from '../lib/i18n';

interface GameLogSession {
  path: string;
  instance_name: string;
  started_at: string;
  size_bytes: number;
}

export function GameLogs() {
  const t = useT();
  const [sessions, setSessions] = useState<GameLogSession[]>([]);
  const [selectedPath, setSelectedPath] = useState<string | null>(null);
  const [content, setContent] = useState('');
  const [loading, setLoading] = useState(true);
  const [loadingContent, setLoadingContent] = useState(false);
  const [currentGameLog, setCurrentGameLog] = useState<string | null>(null);
  const bottomRef = useRef<HTMLDivElement>(null);
  const autoScrollRef = useRef(true);
  const prevCurrentLogRef = useRef<string | null>(null);

  const loadSessions = useCallback(async () => {
    try {
      const [list, current] = await Promise.all([
        invoke<GameLogSession[]>('cmd_list_game_logs'),
        invoke<string | null>('cmd_get_current_game_log'),
      ]);
      setSessions(list);
      setCurrentGameLog(current);
    } catch {
      // ignore
    } finally {
      setLoading(false);
    }
  }, []);

  const loadContent = useCallback(async (path: string) => {
    setLoadingContent(true);
    try {
      const text = await invoke<string>('cmd_read_game_log', { path });
      setContent(text);
    } catch {
      setContent('');
    } finally {
      setLoadingContent(false);
    }
  }, []);

  useEffect(() => {
    loadSessions();
    const interval = setInterval(loadSessions, 5000);
    return () => clearInterval(interval);
  }, [loadSessions]);

  // When a new MC session starts (currentGameLog changes to a new path), auto-select it
  useEffect(() => {
    if (currentGameLog && currentGameLog !== prevCurrentLogRef.current) {
      setSelectedPath(currentGameLog);
      prevCurrentLogRef.current = currentGameLog;
    }
  }, [currentGameLog]);

  // When selected path changes, load content
  useEffect(() => {
    if (selectedPath) {
      loadContent(selectedPath);
    }
  }, [selectedPath, loadContent]);

  // Listen for live log updates when viewing the CURRENT (live) game log
  useEffect(() => {
    if (!currentGameLog || selectedPath !== currentGameLog) return;

    const unlisten = listen<{ level: string; source: string; message: string }>('log_message', (event) => {
      if (event.payload.source === 'minecraft') {
        setContent((prev) => {
          const line = event.payload.message;
          const updated = prev ? prev + '\n' + line : line;
          const lines = updated.split('\n');
          if (lines.length > 5000) {
            return lines.slice(lines.length - 5000).join('\n');
          }
          return updated;
        });
      }
    });

    return () => { unlisten.then((f) => f()); };
  }, [currentGameLog, selectedPath]);

  // Listen for launch_complete to refresh sessions and current log state
  useEffect(() => {
    const unlisten = listen<{ status: string }>('launch_complete', () => {
      loadSessions();
    });
    return () => { unlisten.then((f) => f()); };
  }, [loadSessions]);

  // Auto-scroll
  useEffect(() => {
    if (autoScrollRef.current && bottomRef.current) {
      bottomRef.current.scrollIntoView({ behavior: 'smooth' });
    }
  }, [content]);

  const handleScroll = (e: React.UIEvent<HTMLDivElement>) => {
    const el = e.currentTarget;
    autoScrollRef.current = el.scrollHeight - el.scrollTop - el.clientHeight < 50;
  };

  const formatSize = (bytes: number): string => {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  };

  const lines = content ? content.split('\n') : [];

  return (
    <div className="page animate-fade-in" style={{ display: 'flex', flexDirection: 'column', height: '100%' }}>
      <div className="page__header" style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', flexShrink: 0 }}>
        <div>
          <h1 className="page__title">{t('game_logs.title')}</h1>
          <p className="page__subtitle">{t('game_logs.subtitle')}</p>
        </div>
        <div style={{ display: 'flex', gap: 'var(--space-sm)', alignItems: 'center' }}>
          {/* Session selector */}
          {sessions.length > 0 && (
            <div className="game-logs-select-wrapper">
              <select
                className="game-logs-select"
                value={selectedPath || ''}
                onChange={(e) => setSelectedPath(e.target.value)}
              >
                {sessions.map((s) => {
                  const isCurrent = currentGameLog === s.path;
                  const label = `${s.instance_name} — ${s.started_at} (${formatSize(s.size_bytes)})`;
                  return (
                    <option key={s.path} value={s.path}>
                      {isCurrent ? '● ' : ''}{label}
                    </option>
                  );
                })}
              </select>
              <svg className="game-logs-select-chevron" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                <polyline points="6 9 12 15 18 9" />
              </svg>
            </div>
          )}
          <button className="btn btn--ghost btn--sm" onClick={() => {
            navigator.clipboard.writeText(lines.join('\n'));
          }}>
            {t('common.copy_all')}
          </button>
          <button className="btn btn--ghost btn--sm" onClick={() => setContent('')}>
            {t('common.clear')}
          </button>
        </div>
      </div>

      {loading ? (
        <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-sm)', padding: 'var(--space-lg)', color: 'var(--text-secondary)' }}>
          {t('common.loading')}
        </div>
      ) : sessions.length === 0 ? (
        <div style={{ color: 'var(--text-tertiary)', textAlign: 'center', paddingTop: 'var(--space-2xl)' }}>
          {t('game_logs.empty')}
        </div>
      ) : (
        <div className="log-container" style={{
          flex: 1,
          overflowY: 'auto',
          background: 'var(--surface-elevated)',
          borderRadius: 'var(--radius-lg)',
          padding: 'var(--space-md)',
          fontFamily: "'Cascadia Code', 'Fira Code', 'JetBrains Mono', monospace",
          fontSize: 'var(--font-size-xs)',
          lineHeight: 1.6,
        }} onScroll={handleScroll}>
          {loadingContent ? (
            <div style={{ color: 'var(--text-secondary)', textAlign: 'center', paddingTop: 'var(--space-2xl)' }}>
              {t('common.loading')}
            </div>
          ) : lines.length === 0 ? (
            <div style={{ color: 'var(--text-tertiary)', textAlign: 'center', paddingTop: 'var(--space-2xl)' }}>
              {t('game_logs.no_content')}
            </div>
          ) : (
            <>
              {lines.map((line, i) => (
                <div key={i} className="log-line" style={{
                  padding: '1px var(--space-sm)',
                  whiteSpace: 'pre-wrap',
                  wordBreak: 'break-all',
                  color: 'var(--text-primary)',
                  borderRadius: 'var(--radius-sm)',
                  userSelect: 'text',
                }}>
                  {line}
                </div>
              ))}
              <div ref={bottomRef} />
            </>
          )}
        </div>
      )}
    </div>
  );
}

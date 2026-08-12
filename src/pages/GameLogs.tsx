import { useEffect, useRef, useState, useCallback } from 'react';
import { createPortal } from 'react-dom';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { useT } from '../lib/i18n';
import { useGameLogStore } from '../stores/gameLogStore';
import { addToast } from '../components/ui/Toast';

interface GameLogSession {
  path: string;
  instance_name: string;
  started_at: string;
  size_bytes: number;
}

export function GameLogs() {
  const t = useT();
  const [sessions, setSessions] = useState<GameLogSession[]>([]);
  const selectedPath = useGameLogStore((s) => s.selectedPath);
  const setSelectedPath = useGameLogStore((s) => s.setSelectedPath);
  const [content, setContent] = useState('');
  const [loading, setLoading] = useState(true);
  const [loadingContent, setLoadingContent] = useState(false);
  const [currentGameLog, setCurrentGameLog] = useState<string | null>(null);
  const [showDeleteConfirm, setShowDeleteConfirm] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const [pickerOpen, setPickerOpen] = useState(false);
  const [pickerPos, setPickerPos] = useState<{ top: number; left: number; width: number } | null>(null);
  const pickerBtnRef = useRef<HTMLButtonElement>(null);
  const bottomRef = useRef<HTMLDivElement>(null);
  const autoScrollRef = useRef(true);
  const contentRef = useRef('');
  contentRef.current = content;

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

  const loadSessions = useCallback(async () => {
    try {
      const [list, current] = await Promise.all([
        invoke<GameLogSession[]>('cmd_list_game_logs'),
        invoke<string | null>('cmd_get_current_game_log'),
      ]);
      setSessions(list);
      setCurrentGameLog(current);
      // Auto-select: prefer current game log, then most recent session
      if (current && !useGameLogStore.getState().selectedPath) {
        setSelectedPath(current);
      } else if (list.length > 0 && !useGameLogStore.getState().selectedPath) {
        setSelectedPath(list[0].path);
      }
    } catch {
      // ignore
    } finally {
      setLoading(false);
    }
  }, [setSelectedPath]);

  // Load sessions on mount + every 5s
  useEffect(() => {
    loadSessions();
    const interval = setInterval(loadSessions, 5000);
    return () => clearInterval(interval);
  }, [loadSessions]);

  // Auto-select current game log when it changes (new game starts)
  useEffect(() => {
    if (currentGameLog && currentGameLog !== selectedPath) {
      setSelectedPath(currentGameLog);
    }
  }, [currentGameLog, selectedPath, setSelectedPath]);

  // When selected path changes, load content from disk
  useEffect(() => {
    if (selectedPath) {
      loadContent(selectedPath);
    }
  }, [selectedPath, loadContent]);

  // Periodically re-read the selected log file from disk
  useEffect(() => {
    if (!selectedPath) return;
    const interval = setInterval(() => {
      invoke<string>('cmd_read_game_log', { path: selectedPath }).then((text) => {
        setContent(text);
      }).catch(() => {});
    }, 2000);
    return () => clearInterval(interval);
  }, [selectedPath]);

  // Listen for game log events
  useEffect(() => {
    let unlistenFn: (() => void) | undefined;
    let cancelled = false;

    const p = listen<{ level: string; source: string; message: string }>('log_message', (event) => {
      if (cancelled) return;
      if (event.payload.source !== 'minecraft' && event.payload.source !== 'launch') return;
      const line = event.payload.message;
      setContent((prev) => {
        const updated = prev ? prev + '\n' + line : line;
        const lines = updated.split('\n');
        if (lines.length > 5000) {
          return lines.slice(lines.length - 5000).join('\n');
        }
        return updated;
      });
    });

    p.then((fn) => {
      if (cancelled) { try { fn(); } catch {} return; }
      unlistenFn = fn;
    });

    return () => {
      cancelled = true;
      try { unlistenFn?.(); } catch {}
    };
  }, []);

  // Listen for launch_complete to refresh sessions
  useEffect(() => {
    const unlisten = listen<{ status: string }>('launch_complete', () => {
      loadSessions();
    });
    return () => { unlisten.then((f) => f()).catch(() => {}); };
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

  const togglePicker = () => {
    if (pickerOpen) {
      setPickerOpen(false);
      return;
    }
    const el = pickerBtnRef.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    setPickerPos({ top: r.bottom + 6, left: r.left, width: r.width });
    setPickerOpen(true);
  };

  const selectSession = (path: string) => {
    setSelectedPath(path);
    setPickerOpen(false);
  };

  // Close the picker dropdown on Escape or window resize
  useEffect(() => {
    if (!pickerOpen) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setPickerOpen(false);
    };
    const onResize = () => setPickerOpen(false);
    window.addEventListener('keydown', onKey);
    window.addEventListener('resize', onResize);
    return () => {
      window.removeEventListener('keydown', onKey);
      window.removeEventListener('resize', onResize);
    };
  }, [pickerOpen]);

  const selectedSession = sessions.find((s) => s.path === selectedPath) || null;

  const handleDelete = async () => {
    if (!selectedPath) return;
    setDeleting(true);
    try {
      await invoke('cmd_delete_game_log', { path: selectedPath });
      addToast(t('game_logs.delete_success'), 'success');
      setSelectedPath(null);
      setContent('');
      setShowDeleteConfirm(false);
      await loadSessions();
    } catch (e: any) {
      addToast(t('game_logs.delete_error', { error: e.toString() }), 'error');
    } finally {
      setDeleting(false);
    }
  };

  const formatSize = (bytes: number): string => {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  };

  const lines = content ? content.split('\n') : [];

  const getLineLevel = (line: string): string => {
    const upper = line.toUpperCase();
    if (/\[ERROR\]|\/ERROR\]/.test(line)) return 'error';
    if (/\[WARN\]|\/WARN\]/.test(line)) return 'warn';
    if (/\[DEBUG\]|\/DEBUG\]/.test(line)) return 'debug';
    if (/\bEXCEPTION\b/.test(upper) || /\bFATAL\b/.test(upper)) return 'error';
    if (/exit code [1-9]/.test(line) || /exit code \d{2,}/.test(line)) return 'error';
    if (/FAILED/i.test(line) || /\bERROR\b/i.test(line)) return 'error';
    if (/WARNING/i.test(line)) return 'warn';
    return '';
  };

  const getLineColor = (level: string): string => {
    switch (level) {
      case 'error': return 'var(--color-danger)';
      case 'warn': return 'var(--color-warning)';
      case 'debug': return 'var(--text-tertiary)';
      default: return 'var(--text-primary)';
    }
  };

  return (
    <div className="page animate-fade-in" style={{ display: 'flex', flexDirection: 'column', height: '100%' }}>
      <div className="page__header" style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', flexShrink: 0 }}>
        <div>
          <h1 className="page__title">{t('game_logs.title')}</h1>
          <p className="page__subtitle">{t('game_logs.subtitle')}</p>
        </div>
        <div style={{ display: 'flex', gap: 'var(--space-sm)', alignItems: 'center' }}>
          {sessions.length > 0 && (
            <button
              ref={pickerBtnRef}
              className="game-logs-picker-trigger"
              onClick={togglePicker}
              title={selectedSession ? `${selectedSession.instance_name} — ${selectedSession.started_at}` : ''}
            >
              <span className={`game-logs-picker-dot${currentGameLog && selectedPath === currentGameLog ? ' game-logs-picker-dot--live' : ''}`} />
              <span className="game-logs-picker-trigger-instance">
                {selectedSession ? selectedSession.instance_name : ''}
              </span>
              <span className="game-logs-picker-trigger-time">
                {selectedSession ? selectedSession.started_at.slice(5, 16) : ''}
              </span>
              <span className="game-logs-picker-trigger-size">
                {selectedSession ? formatSize(selectedSession.size_bytes) : ''}
              </span>
              <svg className={`game-logs-picker-chevron${pickerOpen ? ' game-logs-picker-chevron--open' : ''}`} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                <polyline points="6 9 12 15 18 9" />
              </svg>
            </button>
          )}

          {pickerOpen && pickerPos && createPortal(
            <>
              <div className="game-logs-picker-overlay" onClick={() => setPickerOpen(false)} />
              <div className="game-logs-picker-panel" style={{ top: pickerPos.top, left: pickerPos.left, minWidth: pickerPos.width }}>
                <div className="game-logs-picker-header">
                  {t('game_logs.recent_sessions')}
                </div>
                {sessions.map((s) => {
                  const isCurrent = currentGameLog === s.path;
                  const isSelected = selectedPath === s.path;
                  return (
                    <div
                      key={s.path}
                      className={`game-logs-picker-row${isSelected ? ' game-logs-picker-row--selected' : ''}`}
                      onClick={() => selectSession(s.path)}
                    >
                      <span className={`game-logs-picker-dot${isCurrent ? ' game-logs-picker-dot--live' : ''}`} />
                      <span className="game-logs-picker-row-instance">{s.instance_name}</span>
                      {isCurrent && (
                        <span className="game-logs-picker-badge">{t('game_logs.active')}</span>
                      )}
                      <span className="game-logs-picker-row-meta">
                        {s.started_at} · {formatSize(s.size_bytes)}
                      </span>
                    </div>
                  );
                })}
              </div>
            </>,
            document.body,
          )}
          <button className="btn btn--ghost btn--sm" onClick={() => {
            navigator.clipboard.writeText(lines.join('\n'));
          }}>
            {t('common.copy_all')}
          </button>
          {selectedPath && currentGameLog !== selectedPath && (
            <button className="btn btn--ghost btn--sm" onClick={() => setShowDeleteConfirm(true)}>
              {t('common.delete')}
            </button>
          )}
        </div>
      </div>

      {showDeleteConfirm && (
        <div style={{ position: 'fixed', inset: 0, zIndex: 10000, display: 'flex', alignItems: 'center', justifyContent: 'center', background: 'rgba(0,0,0,0.6)' }} onClick={() => !deleting && setShowDeleteConfirm(false)}>
          <div style={{ background: 'var(--bg-elevated)', border: '1px solid var(--surface-border)', borderRadius: 'var(--radius-lg)', padding: 'var(--space-xl)', maxWidth: 400, width: '90%' }} onClick={(e) => e.stopPropagation()}>
            <h3 style={{ margin: '0 0 var(--space-sm)', fontSize: 'var(--font-size-lg)', fontWeight: 700 }}>{t('game_logs.delete_title')}</h3>
            <p style={{ margin: '0 0 var(--space-xl)', fontSize: 'var(--font-size-sm)', color: 'var(--text-secondary)' }}>{t('game_logs.delete_confirm')}</p>
            <div style={{ display: 'flex', gap: 'var(--space-sm)', justifyContent: 'flex-end' }}>
              <button className="btn btn--ghost btn--sm" onClick={() => setShowDeleteConfirm(false)} disabled={deleting}>{t('common.cancel')}</button>
              <button className="btn btn--danger btn--sm" onClick={handleDelete} disabled={deleting}>{deleting ? t('common.loading') : t('common.confirm')}</button>
            </div>
          </div>
        </div>
      )}

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
          background: 'var(--bg-primary)',
          borderRadius: 'var(--radius-lg)',
          padding: 'var(--space-md)',
          fontFamily: "'Cascadia Code', 'Fira Code', 'JetBrains Mono', monospace",
          fontSize: 'var(--font-size-xs)',
          lineHeight: 1.6,
        }} onScroll={handleScroll}>
          {loadingContent || (selectedPath && lines.length === 0 && !loading) ? (
            <div style={{ color: 'var(--text-secondary)', textAlign: 'center', paddingTop: 'var(--space-2xl)' }}>
              {t('common.loading')}
            </div>
          ) : lines.length === 0 ? (
            <div style={{ color: 'var(--text-tertiary)', textAlign: 'center', paddingTop: 'var(--space-2xl)' }}>
              {t('game_logs.no_content')}
            </div>
          ) : (
            <>
              {lines.map((line, i) => {
                const level = getLineLevel(line);
                return (
                <div key={i} className="log-line" style={{
                  padding: '1px var(--space-sm)',
                  whiteSpace: 'pre-wrap',
                  wordBreak: 'break-all',
                  color: getLineColor(level),
                  borderRadius: 'var(--radius-sm)',
                  userSelect: 'text',
                  background: level === 'error' ? 'rgba(255, 60, 60, 0.06)' : level === 'warn' ? 'rgba(255, 180, 40, 0.06)' : 'transparent',
                }}>
                  {line}
                </div>
                );
              })}
              <div ref={bottomRef} />
            </>
          )}
        </div>
      )}
    </div>
  );
}

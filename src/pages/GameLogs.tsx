import { useEffect, useRef, useState, useCallback } from 'react';
import { createPortal } from 'react-dom';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { useT } from '../lib/i18n';
import { formatSize } from '../lib/format';

interface GameLogSession {
  path: string;
  instance_name: string;
  started_at: string;
  size_bytes: number;
}

// Every run stays on disk forever; the launcher UI shows the most recent
// ones only (older files remain available via "Open logs folder").
const MAX_SHOWN_RUNS = 7;

// Prism Launcher's default console limit (ConsoleMaxLines = 100000):
// the backend returns at most this many most-recent lines of the
// unified session log, older ones are dropped.
const MAX_DISPLAY_LINES = 100_000;

export function GameLogs() {
  const t = useT();
  // Unified session log content: launcher setup messages AND the game's own
  // stdout/stderr interleaved in true chronological order (the backend pipes
  // are drained into the same file as they arrive — Prism-style console).
  const [content, setContent] = useState('');
  const [loadingContent, setLoadingContent] = useState(true);

  // Recent runs (newest first), the path of the currently RUNNING run and
  // which session file is on screen (null = follow the newest one, i.e.
  // live tail while a game runs). Launching the same instance again is just
  // another entry in this flat list — no per-instance sub-history.
  const [sessionsList, setSessionsList] = useState<GameLogSession[]>([]);
  const [currentPath, setCurrentPath] = useState<string | null>(null);
  const selectedSessionRef = useRef<string | null>(null);
  const [selectedSessionPath, setSelectedSessionPath] = useState<string | null>(null);

  const [runsOpen, setRunsOpen] = useState(false);
  const [runsPos, setRunsPos] = useState<{ top: number; left: number; width: number } | null>(null);
  const runsBtnRef = useRef<HTMLButtonElement>(null);

  // Chronological console: oldest lines at the TOP, fresh ones appended at
  // the BOTTOM (like Prism's console). The view follows the bottom while
  // lines stream in, unless the user scrolled up to read.
  const logContainerRef = useRef<HTMLDivElement>(null);
  const atBottomRef = useRef(true);
  // Guards against out-of-order async commits between overlapping polls.
  const loadTokenRef = useRef(0);

  const loadContent = useCallback(async (showLoading: boolean) => {
    const token = ++loadTokenRef.current;
    if (showLoading) setLoadingContent(true);
    try {
      const [all, cur] = await Promise.all([
        invoke<GameLogSession[]>('cmd_list_game_logs'),
        invoke<string | null>('cmd_get_current_game_log').catch(() => null),
      ]);
      if (token !== loadTokenRef.current) return;
      setSessionsList(all.slice(0, MAX_SHOWN_RUNS));
      setCurrentPath(cur);

      // Show the manually picked run unless it no longer exists on disk
      // (deleted manually) — then fall back to the newest.
      let target = selectedSessionRef.current;
      if (target && !all.some((s) => s.path === target)) {
        selectedSessionRef.current = null;
        setSelectedSessionPath(null);
        target = null;
      }
      target = target ?? all[0]?.path ?? null;

      if (!target) {
        setContent('');
        return;
      }
      const text = await invoke<string>('cmd_read_game_log', {
        path: target,
        maxLines: MAX_DISPLAY_LINES,
      });
      if (token !== loadTokenRef.current) return;
      setContent(text);
    } catch {
      if (token === loadTokenRef.current) setContent('');
    } finally {
      if (showLoading && token === loadTokenRef.current) setLoadingContent(false);
    }
  }, []);

  useEffect(() => {
    loadContent(true);
    const interval = setInterval(() => loadContent(false), 1000);
    return () => clearInterval(interval);
  }, [loadContent]);

  // Session boundary: "Preparing to launch: <instance>" is the very first
  // line the backend emits for every run. Follow the fresh run immediately.
  useEffect(() => {
    let unlistenFn: (() => void) | undefined;
    let cancelled = false;

    const p = listen<{ level: string; source: string; message: string }>('log_message', (event) => {
      if (cancelled) return;
      if (event.payload.source !== 'launch') return;
      if (!event.payload.message.startsWith('Preparing to launch:')) return;
      atBottomRef.current = true;
      selectedSessionRef.current = null;
      setSelectedSessionPath(null);
      setContent('');
      loadContent(false);
    });

    p.then((fn) => {
      if (cancelled) { try { fn(); } catch {} return; }
      unlistenFn = fn;
    });

    return () => {
      cancelled = true;
      try { unlistenFn?.(); } catch {}
    };
  }, [loadContent]);

  useEffect(() => {
    if (atBottomRef.current && logContainerRef.current) {
      const el = logContainerRef.current;
      el.scrollTo({ top: el.scrollHeight });
    }
  }, [content]);

  const handleScroll = (e: React.UIEvent<HTMLDivElement>) => {
    const el = e.currentTarget;
    atBottomRef.current = el.scrollTop + el.clientHeight >= el.scrollHeight - 50;
  };

  const toggleRuns = () => {
    if (runsOpen) {
      setRunsOpen(false);
      return;
    }
    const el = runsBtnRef.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    setRunsPos({ top: r.bottom + 6, left: Math.max(8, r.right - 360), width: 360 });
    setRunsOpen(true);
  };

  const selectSession = (path: string) => {
    selectedSessionRef.current = path;
    setSelectedSessionPath(path);
    atBottomRef.current = true;
    setRunsOpen(false);
    loadContent(false);
  };

  useEffect(() => {
    if (!runsOpen) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setRunsOpen(false);
    };
    const onResize = () => setRunsOpen(false);
    window.addEventListener('keydown', onKey);
    window.addEventListener('resize', onResize);
    return () => {
      window.removeEventListener('keydown', onKey);
      window.removeEventListener('resize', onResize);
    };
  }, [runsOpen]);

  // The file currently shown: manual pick or the newest run.
  const shownSession =
    sessionsList.find((s) => s.path === selectedSessionPath) || sessionsList[0] || null;

  // Chronological order (oldest → newest): render the file as-is, the view
  // auto-scrolls to the bottom. The backend already caps the tail.
  const displayLines = content ? content.split('\n') : [];

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
          {/* Recent runs picker: last launches of ANY instance, newest first */}
          <button
            ref={runsBtnRef}
            className="game-logs-picker-trigger"
            onClick={toggleRuns}
            title={shownSession ? `${shownSession.instance_name} — ${shownSession.started_at}` : ''}
          >
            <span className={`game-logs-picker-dot${shownSession?.path === currentPath ? ' game-logs-picker-dot--live' : ''}`} />
            <span className="game-logs-picker-trigger-instance">
              {shownSession ? shownSession.instance_name : ''}
            </span>
            <span className="game-logs-picker-trigger-time">
              {shownSession ? shownSession.started_at.slice(5, 16) : ''}
            </span>
            <span className="game-logs-picker-trigger-size">
              {shownSession ? formatSize(shownSession.size_bytes) : ''}
            </span>
            <svg className={`game-logs-picker-chevron${runsOpen ? ' game-logs-picker-chevron--open' : ''}`} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <polyline points="6 9 12 15 18 9" />
            </svg>
          </button>

          {runsOpen && runsPos && createPortal(
            <>
              <div className="game-logs-picker-overlay" onClick={() => setRunsOpen(false)} />
              <div className="game-logs-picker-panel" style={{ top: runsPos.top, left: runsPos.left, minWidth: runsPos.width }}>
                <div className="game-logs-picker-header">
                  {t('game_logs.runs')}
                </div>
                {sessionsList.length === 0 ? (
                  <div className="game-logs-picker-row">
                    <span className="game-logs-picker-row-meta">{t('game_logs.no_latest')}</span>
                  </div>
                ) : (
                  sessionsList.map((s) => (
                    <div
                      key={s.path}
                      className={`game-logs-picker-row${(shownSession?.path ?? '') === s.path ? ' game-logs-picker-row--selected' : ''}`}
                      onClick={() => selectSession(s.path)}
                    >
                      <span className={`game-logs-picker-dot${s.path === currentPath ? ' game-logs-picker-dot--live' : ''}`} />
                      <span className="game-logs-picker-row-instance">{s.instance_name}</span>
                      {s.path === currentPath && (
                        <span className="game-logs-picker-badge">{t('game_logs.active')}</span>
                      )}
                      <span className="game-logs-picker-row-meta">
                        {s.started_at} · {formatSize(s.size_bytes)}
                      </span>
                    </div>
                  ))
                )}
              </div>
            </>,
            document.body,
          )}

          <button className="btn btn--ghost btn--sm" onClick={() => {
            navigator.clipboard.writeText(displayLines.join('\n'));
          }}>
            {t('common.copy_all')}
          </button>
          <button className="btn btn--ghost btn--sm" onClick={async () => {
            try {
              await invoke('cmd_open_game_logs_root');
            } catch (e: any) {
              console.error(String(e));
            }
          }}>
            {t('game_logs.open_folder')}
          </button>
        </div>
      </div>

      <div className="log-container" ref={logContainerRef} style={{
        flex: 1,
        overflowY: 'auto',
        background: 'var(--bg-primary)',
        borderRadius: 'var(--radius-lg)',
        padding: 'var(--space-md)',
        fontFamily: "'Cascadia Code', 'Fira Code', 'JetBrains Mono', monospace",
        fontSize: 'var(--font-size-xs)',
        lineHeight: 1.6,
      }} onScroll={handleScroll}>
        {loadingContent || displayLines.length === 0 ? (
          <div style={{ color: 'var(--text-secondary)', textAlign: 'center', paddingTop: 'var(--space-2xl)' }}>
            {t('game_logs.no_latest')}
          </div>
        ) : (
          displayLines.map((line, i) => {
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
                // Skip layout/paint for off-screen lines so a 100k-line
                // log (Prism-sized) doesn't freeze the UI.
                contentVisibility: 'auto',
                containIntrinsicSize: 'auto 19px',
              }}>
                {line}
              </div>
            );
          })
        )}
      </div>
    </div>
  );
}

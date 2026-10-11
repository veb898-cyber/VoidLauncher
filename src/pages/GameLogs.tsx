import { useEffect, useMemo, useRef, useState, useCallback } from 'react';
import { createPortal } from 'react-dom';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { AlertTriangle, HelpCircle, XCircle, FileText, Copy, FileSearch, FolderOpen, Loader2 } from 'lucide-react';
import { useT } from '../lib/i18n';
import { formatSize } from '../lib/format';
import { addToast } from '../components/ui/Toast';
import { Tooltip } from '../components/ui/Tooltip';
import { CRASH_KEYS, VERDICT_KEYS, type CrashAnalysis } from '../lib/diagnostics';
import {
  LogToolbar, allLevelsOn, toLevelFilter,
  type LevelFilterState, type LogLevelFilter,
} from '../components/LogToolbar';

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

/** Classify a raw log line into a severity bucket (module scope so the filter
    memo can depend on it without re-creating it every render). */
function getLineLevel(line: string): string {
  const upper = line.toUpperCase();
  if (/\[ERROR\]|\/ERROR\]/.test(line)) return 'error';
  if (/\[WARN\]|\/WARN\]/.test(line)) return 'warn';
  if (/\[DEBUG\]|\/DEBUG\]/.test(line)) return 'debug';
  if (/\bEXCEPTION\b/.test(upper) || /\bFATAL\b/.test(upper)) return 'error';
  if (/exit code [1-9]/.test(line) || /exit code \d{2,}/.test(line)) return 'error';
  if (/FAILED/i.test(line) || /\bERROR\b/i.test(line)) return 'error';
  if (/WARNING/i.test(line)) return 'warn';
  return '';
}

function getLineColor(level: string): string {
  switch (level) {
    case 'error': return 'var(--color-danger)';
    case 'warn': return 'var(--color-warning)';
    case 'debug': return 'var(--text-tertiary)';
    default: return 'var(--text-primary)';
  }
}

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

  // Crash analysis for the instance of the shown session.
  const [analysis, setAnalysis] = useState<{ instance: string; data: CrashAnalysis } | null>(null);
  const [analyzing, setAnalyzing] = useState(false);

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
    // Anchored to the button's LEFT edge, with the width clamped so the panel
    // still never reaches past the right edge of the window.
    const left = Math.max(8, r.left);
    setRunsPos({ top: r.bottom + 6, left, width: Math.min(360, window.innerWidth - left - 8) });
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

  const analyzeCrash = useCallback(async () => {
    if (!shownSession) return;
    const instance = shownSession.instance_name;
    setAnalyzing(true);
    try {
      // Pass the session path so the backend analyzes THIS run (crash
      // reports are filtered to its time window) — not "the newest crash
      // ever", which could be from an older launch.
      const data = await invoke<CrashAnalysis>('cmd_analyze_crash', {
        instanceName: instance,
        sessionPath: shownSession.path,
      });
      if (data.source === 'clean' || data.source === 'none') {
        setAnalysis(null);
        addToast(t('game_logs.no_crash'), 'info');
        return;
      }
      setAnalysis({ instance, data });
    } catch (e) {
      addToast(e instanceof Error ? e.message : String(e), 'error');
    } finally {
      setAnalyzing(false);
    }
  }, [shownSession, t]);

  const analysisVisible =
    analysis !== null && shownSession !== null && analysis.instance === shownSession.instance_name;

  const copyAnalysis = async () => {
    if (!analysis) return;
    const entry = CRASH_KEYS[analysis.data.kind] ?? CRASH_KEYS.unknown;
    const lines = [
      t('diag.crash_heading'),
      `${t('diag.verdict_label')}: ${VERDICT_KEYS[analysis.data.verdict] ? t(VERDICT_KEYS[analysis.data.verdict]) : analysis.data.verdict}`,
      t(entry.message, { culprit: analysis.data.culprit ?? '—' }),
      entry.hint ? t(entry.hint) : '',
      analysis.data.detail,
      analysis.data.report_path ?? '',
    ].filter(Boolean);
    try {
      await navigator.clipboard.writeText(lines.join('\n'));
      addToast(t('common.copied'), 'success');
    } catch {
      addToast(t('common.copy_failed'), 'error');
    }
  };

  // Chronological order (oldest → newest): render the file as-is, the view
  // auto-scrolls to the bottom. The backend already caps the tail.
  const displayLines = useMemo(() => (content ? content.split('\n') : []), [content]);

  const [query, setQuery] = useState('');
  const [debouncedQuery, setDebouncedQuery] = useState('');
  const [levels, setLevels] = useState<LevelFilterState>(allLevelsOn);

  // Re-classifying 100k lines on every keystroke would stutter, so the query
  // trails the input slightly. Level toggles apply immediately.
  useEffect(() => {
    const id = window.setTimeout(() => setDebouncedQuery(query), 150);
    return () => window.clearTimeout(id);
  }, [query]);

  const needle = debouncedQuery.trim().toLowerCase();
  const filtering = needle !== '' || !levels.error || !levels.warn || !levels.info;

  const visibleLines = useMemo(() => {
    if (!filtering) return displayLines;
    return displayLines.filter((line) => {
      if (!levels[toLevelFilter(getLineLevel(line))]) return false;
      return needle === '' || line.toLowerCase().includes(needle);
    });
  }, [displayLines, filtering, levels, needle]);

  const toggleLevel = (level: LogLevelFilter) => {
    setLevels((prev) => ({ ...prev, [level]: !prev[level] }));
  };

  return (
    <div style={{ display: 'flex', flexDirection: 'column', height: '100%', gap: 'var(--space-md)' }}>
      <LogToolbar
        query={query}
        onQueryChange={setQuery}
        levels={levels}
        onToggleLevel={toggleLevel}
        leading={
          <>
          {/* Recent runs picker: last launches of ANY instance, newest first */}
          <button
            ref={runsBtnRef}
            className="game-logs-picker-trigger"
            onClick={toggleRuns}
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
              <div className="game-logs-picker-panel" style={{ top: runsPos.top, left: runsPos.left, width: runsPos.width }}>
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
          </>
        }
        actions={
          <>
            {/* Icon-only action cluster: with the Russian labels the three
                text buttons + search + filters + run picker overflow 1200px
                and wrapped to a second row. Icons keep it one row; the label
                lives in a tooltip + aria-label. */}
            <Tooltip content={analyzing ? t('game_logs.analyzing') : t('game_logs.analyze')} side="bottom">
              <button
                className="btn btn--secondary btn--sm"
                onClick={analyzeCrash}
                disabled={!shownSession || analyzing}
                aria-label={t('game_logs.analyze')}
              >
                {analyzing
                  ? <Loader2 size={15} style={{ animation: 'spin 1s linear infinite' }} />
                  : <FileSearch size={15} />}
              </button>
            </Tooltip>
            <Tooltip content={t('common.copy_all')} side="bottom">
              <button
                className="btn btn--secondary btn--sm"
                aria-label={t('common.copy_all')}
                onClick={async () => {
                  try {
                    await navigator.clipboard.writeText(visibleLines.join('\n'));
                    addToast(t('common.copied'), 'success');
                  } catch {
                    addToast(t('common.copy_failed'), 'error');
                  }
                }}
              >
                <Copy size={15} />
              </button>
            </Tooltip>
            <Tooltip content={t('game_logs.open_folder')} side="bottom">
              <button
                className="btn btn--secondary btn--sm"
                aria-label={t('game_logs.open_folder')}
                onClick={async () => {
                  try {
                    await invoke('cmd_open_game_logs_root');
                  } catch (e: any) {
                    console.error(String(e));
                  }
                }}
              >
                <FolderOpen size={15} />
              </button>
            </Tooltip>
          </>
        }
      />

      {/* Crash analysis panel (manual button or auto after a failed run) */}
      {analysis && analysisVisible && (() => {
        const a = analysis.data;
        const entry = CRASH_KEYS[a.kind] ?? CRASH_KEYS.unknown;
        const verdictKey = VERDICT_KEYS[a.verdict];
        const verdictColor =
          a.verdict === 'likely' ? 'var(--error, var(--color-danger))'
          : a.verdict === 'possible' ? 'var(--warning, var(--color-warning))'
          : 'var(--text-tertiary)';
        const VerdictIcon =
          a.verdict === 'unknown' ? HelpCircle
          : a.verdict === 'possible' ? AlertTriangle
          : XCircle;
        return (
          <div className="glass-card" style={{ padding: 'var(--space-md) var(--space-lg)', flexShrink: 0 }}>
            <div style={{ display: 'flex', gap: 'var(--space-md)', alignItems: 'flex-start' }}>
              <VerdictIcon size={18} color={verdictColor} style={{ flexShrink: 0, marginTop: 2 }} />
              <div style={{ minWidth: 0, flex: 1 }}>
                <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-sm)', flexWrap: 'wrap', marginBottom: 'var(--space-xs)' }}>
                  <span style={{ fontWeight: 600 }}>{t('diag.crash_heading')}</span>
                  <span style={{
                    fontSize: 'var(--font-size-xs)', fontWeight: 600, color: verdictColor,
                    border: `1px solid ${verdictColor}`, borderRadius: 'var(--radius-sm)',
                    padding: '1px var(--space-sm)',
                  }}>
                    {verdictKey ? t(verdictKey) : a.verdict}
                  </span>
                  <span style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)' }}>
                    {a.source === 'crash-report' ? t('diag.source_crash_report') : t('diag.source_log')}
                  </span>
                </div>

                <div style={{ fontSize: 'var(--font-size-sm)', marginBottom: entry.hint ? '2px' : 'var(--space-sm)' }}>
                  {t(entry.message, { culprit: a.culprit ?? '—' })}
                </div>
                {entry.hint && (
                  <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-secondary)', marginBottom: 'var(--space-sm)' }}>
                    {t(entry.hint)}
                  </div>
                )}

                {a.detail && (
                  <div style={{
                    fontFamily: 'var(--font-mono)', fontSize: 'var(--font-size-xs)',
                    color: 'var(--text-secondary)', background: 'var(--surface-glass)',
                    border: '1px solid var(--surface-border)',
                    borderRadius: 'var(--radius-sm)', padding: 'var(--space-sm)',
                    whiteSpace: 'pre-wrap', wordBreak: 'break-word', marginBottom: 'var(--space-sm)',
                    maxHeight: 160, overflowY: 'auto', userSelect: 'text',
                  }}>
                    {a.detail}
                  </div>
                )}

                <div style={{ display: 'flex', gap: 'var(--space-sm)', flexWrap: 'wrap' }}>
                  {a.report_path && (
                    <button className="btn btn--secondary btn--sm" onClick={async () => {
                      try {
                        await invoke('cmd_open_folder', { path: a.report_path });
                      } catch (e) {
                        addToast(e instanceof Error ? e.message : String(e), 'error');
                      }
                    }}>
                      <FileText size={14} style={{ marginRight: 6 }} />
                      {t('game_logs.open_report')}
                    </button>
                  )}
                  <button className="btn btn--secondary btn--sm" onClick={copyAnalysis}>
                    <Copy size={14} style={{ marginRight: 6 }} />
                    {t('game_logs.copy_analysis')}
                  </button>
                  <button className="btn btn--ghost btn--sm" onClick={() => setAnalysis(null)}>
                    {t('common.close')}
                  </button>
                </div>
              </div>
            </div>
          </div>
        );
      })()}

      <div className="log-container" ref={logContainerRef} style={{
        flex: 1,
        overflowY: 'auto',
        borderRadius: 'var(--radius-lg)',
        padding: 'var(--space-md)',
        fontFamily: 'var(--font-mono)',
        fontSize: 'var(--font-size-xs)',
        lineHeight: 1.6,
      }} onScroll={handleScroll}>
        {loadingContent || displayLines.length === 0 ? (
          <div style={{ color: 'var(--text-secondary)', textAlign: 'center', paddingTop: 'var(--space-2xl)' }}>
            {t('game_logs.no_latest')}
          </div>
        ) : visibleLines.length === 0 ? (
          <div style={{ color: 'var(--text-tertiary)', textAlign: 'center', paddingTop: 'var(--space-2xl)' }}>
            {t('terminal.no_matches')}
          </div>
        ) : (
          visibleLines.map((line, i) => {
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
        {filtering && visibleLines.length > 0 && (
          <div style={{ color: 'var(--text-tertiary)', textAlign: 'center', paddingTop: 'var(--space-sm)' }}>
            {t('terminal.hidden_count', { count: String(displayLines.length - visibleLines.length) })}
          </div>
        )}
      </div>
    </div>
  );
}

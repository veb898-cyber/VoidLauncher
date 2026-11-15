import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type TextareaHTMLAttributes,
} from 'react';
import { invoke } from '@tauri-apps/api/core';
import { openUrl } from '@tauri-apps/plugin-opener';
import { Copy, ExternalLink, RefreshCw } from 'lucide-react';
import { Modal } from '../ui/Modal';
import { Button } from '../ui/Button';
import { addToast } from '../ui/Toast';
import { useT } from '../../lib/i18n';
import { CHECK_LABELS, ISSUE_KEYS, type DiagReport } from '../../lib/diagnostics';
import { friendlyErrorMessage } from '../../lib/errors';
import {
  buildBugReport,
  buildBugReportIssueUrl,
  bugReportFieldErrors,
  isBugReportComplete,
  parseBugReportPreview,
  REQUIRED_BUG_REPORT_FIELDS,
  resolveReportForCopy,
  type BugReportField,
  type BugReportFields,
  type BugReportIssuePrefill,
  type BugReportSection,
} from '../../lib/bugReport';

interface Props {
  open: boolean;
  onClose: () => void;
  instanceName: string | null;
  diagReport: DiagReport | null;
  onRunDiagnostics?: () => Promise<DiagReport | null>;
}

interface PreparedSection {
  id: string;
  content: string;
}

interface PreparedInstance {
  name: string;
  mc_version: string;
  loader: string;
  loader_version: string | null;
  mod_count: number;
}

interface PreparedReport {
  launcher_version: string;
  windows_version: string | null;
  instance: PreparedInstance | null;
  sections: PreparedSection[];
}

interface LogState {
  loading?: boolean;
  content?: string;
  error?: string;
}

const EMPTY_FIELDS: BugReportFields = {
  summary: '',
  steps: '',
  expected: '',
  actual: '',
  extra: '',
};

const EMPTY_LOG_STATE: LogState = {};

const EMPTY_ISSUE_PREFILL: BugReportIssuePrefill = {
  summary: '',
  steps: '',
  expected: '',
  actual: '',
};

function diagnosticsText(report: DiagReport, t: ReturnType<typeof useT>): string {
  const lines: string[] = [];
  for (const check of report.checks) {
    if (!check.value.trim()) continue;
    const key = CHECK_LABELS[check.id];
    lines.push(`${key ? t(key) : check.id}: ${check.value.trim()}`);
  }
  if (report.issues.length > 0) {
    lines.push('', t('diag.issues_heading'));
    for (const issue of report.issues) {
      const keys = ISSUE_KEYS[issue.id];
      lines.push(`- ${keys ? t(keys.message, issue.params) : issue.id}`);
    }
  }
  return lines.join('\n');
}

async function copyText(text: string) {
  if (navigator.clipboard?.writeText) {
    try {
      await navigator.clipboard.writeText(text);
      return;
    } catch {
      // Permission/OS clipboard failure: use the textarea fallback below.
    }
  }
  const previousFocus = document.activeElement as HTMLElement | null;
  const area = document.createElement('textarea');
  area.value = text;
  area.setAttribute('readonly', '');
  area.style.position = 'fixed';
  area.style.opacity = '0';
  document.body.appendChild(area);
  area.select();
  area.setSelectionRange(0, area.value.length);
  let ok = false;
  try {
    ok = document.execCommand('copy');
  } finally {
    area.remove();
    previousFocus?.focus?.();
  }
  if (!ok) throw new Error('clipboard is unavailable');
}

/**
 * Text field that never shows a manual resize handle: its height follows the
 * wrapped content (including soft line breaks) and shrinks back when text is
 * deleted. `rows` is kept as the minimum height, so an empty field still has
 * the same comfortable size as before. Width is untouched (100% of the
 * form); the modal body provides the scrollbar when the report gets long.
 *
 * `useLayoutEffect` runs on every render: the textarea is controlled, so a
 * value change (typing, deleting, or resetting the form on open) always has a
 * fresh `scrollHeight` to measure before the browser paints.
 */
function AutoResizeTextarea({
  rows = 2,
  style,
  ...props
}: TextareaHTMLAttributes<HTMLTextAreaElement>) {
  const ref = useRef<HTMLTextAreaElement | null>(null);

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    // `height: auto` first, otherwise scrollHeight is clamped to the current
    // height and the field could never shrink.
    el.style.height = 'auto';
    const computed = window.getComputedStyle(el);
    const borderY =
      (parseFloat(computed.borderTopWidth) || 0) +
      (parseFloat(computed.borderBottomWidth) || 0);
    const paddingY =
      (parseFloat(computed.paddingTop) || 0) +
      (parseFloat(computed.paddingBottom) || 0);
    const lineHeight = parseFloat(computed.lineHeight) || 20;
    const minHeight = lineHeight * rows + paddingY + borderY;
    // scrollHeight includes padding but not borders; add them back because
    // `.input` uses border-box. The extra pixel absorbs fractional rounding.
    el.style.height = `${Math.max(minHeight, Math.ceil(el.scrollHeight) + borderY + 1)}px`;
  });

  return (
    <textarea
      ref={ref}
      rows={rows}
      {...props}
      style={{
        resize: 'none',
        overflowX: 'hidden',
        overflowY: 'hidden',
        ...style,
      }}
    />
  );
}

export function BugReportModal({
  open,
  onClose,
  instanceName,
  diagReport,
  onRunDiagnostics,
}: Props) {
  const t = useT();
  const [fields, setFields] = useState<BugReportFields>(EMPTY_FIELDS);
  const [prepared, setPrepared] = useState<PreparedReport | null>(null);
  const [baseSections, setBaseSections] = useState<PreparedSection[]>([]);
  const [sectionEnabled, setSectionEnabled] = useState<Record<string, boolean>>({});
  const [diagnostics, setDiagnostics] = useState<DiagReport | null>(diagReport);
  const [diagnosticsLoading, setDiagnosticsLoading] = useState(false);
  const [logs, setLogs] = useState<Record<string, LogState>>({});
  const [reportText, setReportText] = useState('');
  const previewEditedRef = useRef(false);
  const sessionRef = useRef(0);
  const [preparing, setPreparing] = useState(false);
  const [prepareError, setPrepareError] = useState<string | null>(null);
  const [busyAction, setBusyAction] = useState(false);
  const [touched, setTouched] = useState<Partial<Record<BugReportField, boolean>>>({});
  const errors = bugReportFieldErrors(fields);
  const complete = isBugReportComplete(fields);

  const markTouched = (field: BugReportField) => {
    setTouched((previous) => (previous[field] ? previous : { ...previous, [field]: true }));
  };

  const markAllRequiredTouched = () => {
    setTouched((previous) => {
      const next = { ...previous };
      for (const field of REQUIRED_BUG_REPORT_FIELDS) next[field] = true;
      return next;
    });
  };

  const fieldError = (field: BugReportField): string | null =>
    touched[field] && errors[field] ? t('bug.field_required') : null;

  useEffect(() => {
    if (!open) return;
    const session = ++sessionRef.current;
    setFields(EMPTY_FIELDS);
    setReportText('');
    previewEditedRef.current = false;
    setPrepared(null);
    setBaseSections([]);
    setSectionEnabled({});
    setLogs({});
    setPrepareError(null);
    setDiagnostics(diagReport);
    setTouched({});
    let cancelled = false;
    setPreparing(true);
    (async () => {
      try {
        const data = await invoke<PreparedReport>(
          'cmd_prepare_bug_report',
          { instanceName },
        );
        if (cancelled || sessionRef.current !== session) return;
        setPrepared(data);
        setBaseSections(data.sections);
        setSectionEnabled((previous) => {
          const next = { ...previous };
          for (const section of data.sections) next[section.id] = true;
          if (diagReport) next.diagnostics = true;
          return next;
        });
      } catch (error) {
        if (!cancelled && sessionRef.current === session) {
          setPrepareError(friendlyErrorMessage(error, t));
        }
      } finally {
        if (!cancelled && sessionRef.current === session) setPreparing(false);
      }
    })();
    return () => {
      cancelled = true;
      if (sessionRef.current === session) sessionRef.current += 1;
    };
    // The report is prepared once per modal opening / selected instance.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, instanceName]);

  useEffect(() => {
    setDiagnostics(diagReport);
    if (diagReport) {
      setSectionEnabled((previous) => ({ ...previous, diagnostics: true }));
      if (!previewEditedRef.current) setReportText('');
    } else {
      setSectionEnabled((previous) => ({ ...previous, diagnostics: false }));
    }
  }, [diagReport]);

  const invalidatePreview = () => {
    if (!previewEditedRef.current) setReportText('');
  };

  const setField = (key: keyof BugReportFields, value: string) => {
    setFields((previous) => ({ ...previous, [key]: value }));
    invalidatePreview();
  };

  const loadLog = useCallback(async (kind: 'launcher' | 'game') => {
    const session = sessionRef.current;
    setLogs((previous) => ({
      ...previous,
      [kind]: { ...(previous[kind] ?? EMPTY_LOG_STATE), loading: true, error: undefined },
    }));
    try {
      const content = await invoke<string | null>('cmd_get_bug_report_log', {
        instanceName,
        kind,
      });
      if (sessionRef.current !== session) return;
      setLogs((previous) => ({
        ...previous,
        [kind]: content
          ? { content }
          : { error: t('bug.log_empty') },
      }));
      if (!previewEditedRef.current) setReportText('');
    } catch (error) {
      if (sessionRef.current !== session) return;
      setLogs((previous) => ({
        ...previous,
        [kind]: { error: friendlyErrorMessage(error, t) },
      }));
      if (!previewEditedRef.current) setReportText('');
    }
  }, [instanceName, t]);

  const loadCrash = useCallback(async () => {
    if (!instanceName) return;
    const session = sessionRef.current;
    setLogs((previous) => ({
      ...previous,
      crash: { ...(previous.crash ?? EMPTY_LOG_STATE), loading: true, error: undefined },
    }));
    try {
      const content = await invoke<string | null>('cmd_get_bug_report_crash', {
        instanceName,
      });
      if (sessionRef.current !== session) return;
      setLogs((previous) => ({
        ...previous,
        crash: content
          ? { content }
          : { error: t('bug.crash_empty') },
      }));
      if (!previewEditedRef.current) setReportText('');
    } catch (error) {
      if (sessionRef.current !== session) return;
      setLogs((previous) => ({
        ...previous,
        crash: { error: friendlyErrorMessage(error, t) },
      }));
      if (!previewEditedRef.current) setReportText('');
    }
  }, [instanceName, t]);

  const toggleSection = (id: string, enabled: boolean) => {
    setSectionEnabled((previous) => ({ ...previous, [id]: enabled }));
    invalidatePreview();
  };

  const sectionTitle = (id: string): string => {
    switch (id) {
      case 'environment':
        return t('bug.section_environment');
      case 'instance':
        return t('bug.section_instance');
      case 'diagnostics':
        return t('bug.section_diagnostics');
      case 'launcher':
        return t('bug.section_launcher_log');
      case 'game':
        return t('bug.section_game_log');
      case 'crash':
        return t('bug.section_crash');
      default:
        return id;
    }
  };

  const allSections = (): BugReportSection[] => {
    const result: BugReportSection[] = baseSections.map((section) => ({
      id: section.id,
      title: sectionTitle(section.id),
      content: section.content,
      enabled: sectionEnabled[section.id] ?? true,
    }));
    if (diagnostics) {
      result.push({
        id: 'diagnostics',
        title: sectionTitle('diagnostics'),
        content: diagnosticsText(diagnostics, t),
        enabled: sectionEnabled.diagnostics ?? true,
      });
    }
    for (const kind of ['launcher', 'game', 'crash'] as const) {
      const state = logs[kind];
      if (!state?.content) continue;
      result.push({
        id: kind,
        title: sectionTitle(kind),
        content: state.content,
        enabled: sectionEnabled[kind] ?? false,
      });
    }
    return result;
  };

  const reportLabels = () => ({
    title: t('bug.report_title'),
    summary: t('bug.summary'),
    steps: t('bug.steps'),
    expected: t('bug.expected'),
    actual: t('bug.actual'),
    extra: t('bug.extra'),
  });

  const generate = (): string | null => {
    if (!complete) {
      markAllRequiredTouched();
      return null;
    }
    const report = buildBugReport(fields, allSections(), reportLabels());
    setReportText(report);
    previewEditedRef.current = false;
    return report;
  };

  /**
   * The one place that actually builds the report. Both the Copy button and
   * the “Open GitHub issue” flow go through it, so an incomplete form can
   * never reach the clipboard or the external issue page.
   */
  const prepareReport = (): string | null => {
    const report = resolveReportForCopy(
      reportText,
      fields,
      allSections(),
      reportLabels(),
      previewEditedRef.current,
    );
    if (!report) {
      markAllRequiredTouched();
      return null;
    }
    if (!previewEditedRef.current) setReportText(report);
    return report;
  };

  const performCopy = async (): Promise<boolean> => {
    const report = prepareReport();
    if (!report) return false;
    try {
      await copyText(report);
      addToast(t('bug.copied'), 'success');
      return true;
    } catch (error) {
      addToast(t('bug.copy_error', { error: String(error) }), 'error');
      return false;
    }
  };

  const copyReport = async (): Promise<boolean> => {
    if (busyAction) return false;
    setBusyAction(true);
    try {
      return await performCopy();
    } finally {
      setBusyAction(false);
    }
  };

  /**
   * Build the GitHub form prefill from the same values as the copied report.
   * GitHub issue forms accept field-id query parameters; large sections are
   * shortened by `buildBugReportIssueUrl` before the URL is opened. The full
   * report always stays on the clipboard.
   *
   * If the preview was edited, parse the edited text instead of the form state:
   * otherwise details the user deliberately removed would reappear in the URL.
   * A preview that cannot be parsed returns null and the form opens empty.
   */
  const buildIssuePrefill = (): BugReportIssuePrefill | null => {
    if (previewEditedRef.current && reportText.trim()) {
      const parsed = parseBugReportPreview(reportText, reportLabels(), {
        environment: sectionTitle('environment'),
        instance: sectionTitle('instance'),
        diagnostics: sectionTitle('diagnostics'),
        launcher: sectionTitle('launcher'),
        game: sectionTitle('game'),
        crash: sectionTitle('crash'),
      });
      if (!parsed) return null;
      return {
        summary: parsed.fields.summary,
        steps: parsed.fields.steps,
        expected: parsed.fields.expected,
        actual: parsed.fields.actual,
        extra: parsed.fields.extra,
        launcherVersion: prepared?.launcher_version,
        windowsVersion: prepared?.windows_version,
        minecraftVersion: prepared?.instance?.mc_version,
        loader: prepared?.instance?.loader,
        environment: parsed.sections.environment,
        instance: parsed.sections.instance,
        diagnostics: parsed.sections.diagnostics,
        launcherLog: parsed.sections.launcher,
        gameLog: parsed.sections.game,
        crash: parsed.sections.crash,
      };
    }
    const sections = allSections();
    const enabledContent = (id: string) =>
      sections.find((section) => section.id === id && section.enabled)?.content ?? '';
    return {
      summary: fields.summary,
      steps: fields.steps,
      expected: fields.expected,
      actual: fields.actual,
      extra: fields.extra,
      launcherVersion: prepared?.launcher_version,
      windowsVersion: prepared?.windows_version,
      minecraftVersion: prepared?.instance?.mc_version,
      loader: prepared?.instance?.loader,
      environment: enabledContent('environment'),
      instance: enabledContent('instance'),
      diagnostics: enabledContent('diagnostics'),
      launcherLog: enabledContent('launcher'),
      gameLog: enabledContent('game'),
      crash: enabledContent('crash'),
    };
  };

  const openIssue = async () => {
    if (busyAction) return;
    setBusyAction(true);
    try {
      const report = prepareReport();
      if (!report) return;
      try {
        await copyText(report);
      } catch (error) {
        addToast(t('bug.copy_error', { error: String(error) }), 'error');
        return;
      }
      const prefill = buildIssuePrefill();
      const { url, shortened } = buildBugReportIssueUrl(prefill ?? EMPTY_ISSUE_PREFILL);
      await openUrl(url);
      if (!prefill) {
        addToast(t('bug.issue_opened_manual'), 'warning');
      } else {
        addToast(
          t(shortened ? 'bug.issue_opened_shortened' : 'bug.issue_opened'),
          shortened ? 'warning' : 'success',
        );
      }
    } catch (error) {
      addToast(t('bug.open_error', { error: String(error) }), 'error');
    } finally {
      setBusyAction(false);
    }
  };

  const runDiagnostics = async () => {
    if (!onRunDiagnostics || diagnosticsLoading) return;
    setDiagnosticsLoading(true);
    try {
      const report = await onRunDiagnostics();
      if (report) {
        setDiagnostics(report);
        setSectionEnabled((previous) => ({ ...previous, diagnostics: true }));
        invalidatePreview();
      }
    } catch (error) {
      addToast(t('bug.diagnostics_error', { error: String(error) }), 'error');
    } finally {
      setDiagnosticsLoading(false);
    }
  };

  const requiredMark = (
    <span aria-hidden="true" style={{ color: 'var(--error)' }}> *</span>
  );
  const optionalMark = (
    <span style={{ color: 'var(--text-tertiary)', fontWeight: 400 }}>
      {' '}({t('bug.optional')})
    </span>
  );

  return (
    <Modal
      open={open}
      onClose={onClose}
      title={t('bug.title')}
      maxWidth={820}
      footer={
        <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 'var(--space-sm)', flexWrap: 'wrap' }}>
          <Button variant="ghost" onClick={onClose} disabled={busyAction}>{t('common.cancel')}</Button>
          <Button variant="secondary" onClick={copyReport} disabled={busyAction || preparing || !complete}>
            <Copy size={14} /> {t('bug.copy')}
          </Button>
          <Button onClick={openIssue} disabled={busyAction || preparing || !complete}>
            <ExternalLink size={14} /> {t('bug.open_github')}
          </Button>
        </div>
      }
    >
      <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--space-md)' }}>
        <div style={{
          padding: 'var(--space-sm) var(--space-md)',
          border: '1px solid var(--surface-border)',
          borderRadius: 'var(--radius-md)',
          color: 'var(--text-secondary)',
          fontSize: 'var(--font-size-sm)',
          lineHeight: 1.5,
        }}>
          {t('bug.privacy_warning')}
        </div>

        <div style={{ fontSize: 'var(--font-size-sm)', color: 'var(--text-secondary)' }}>
          {t('bug.required_hint')}
        </div>

        {prepareError && (
          <div style={{ color: 'var(--color-danger)', fontSize: 'var(--font-size-sm)' }}>
            {t('bug.prepare_error', { error: prepareError })}
          </div>
        )}

        <div style={{ display: 'grid', gap: 'var(--space-sm)' }}>
          <label className="input__label" htmlFor="bug-summary">
            {t('bug.summary')}{requiredMark}
          </label>
          <AutoResizeTextarea id="bug-summary" className="input" rows={3}
            aria-required="true"
            aria-invalid={touched.summary && !!errors.summary}
            aria-describedby={fieldError('summary') ? 'bug-summary-error' : undefined}
            placeholder={t('bug.summary_placeholder')}
            onBlur={() => markTouched('summary')}
            value={fields.summary} onChange={(e) => setField('summary', e.target.value)} />
          {fieldError('summary') && (
            <div id="bug-summary-error" role="alert" style={{ color: 'var(--error)', fontSize: 'var(--font-size-sm)' }}>
              {t('bug.field_required')}
            </div>
          )}
          <label className="input__label" htmlFor="bug-steps">
            {t('bug.steps')}{requiredMark}
          </label>
          <AutoResizeTextarea id="bug-steps" className="input" rows={3}
            aria-required="true"
            aria-invalid={touched.steps && !!errors.steps}
            aria-describedby={fieldError('steps') ? 'bug-steps-error' : undefined}
            placeholder={t('bug.steps_placeholder')}
            onBlur={() => markTouched('steps')}
            value={fields.steps} onChange={(e) => setField('steps', e.target.value)} />
          {fieldError('steps') && (
            <div id="bug-steps-error" role="alert" style={{ color: 'var(--error)', fontSize: 'var(--font-size-sm)' }}>
              {t('bug.field_required')}
            </div>
          )}
          <label className="input__label" htmlFor="bug-expected">
            {t('bug.expected')}{requiredMark}
          </label>
          <AutoResizeTextarea id="bug-expected" className="input" rows={2}
            aria-required="true"
            aria-invalid={touched.expected && !!errors.expected}
            aria-describedby={fieldError('expected') ? 'bug-expected-error' : undefined}
            placeholder={t('bug.expected_placeholder')}
            onBlur={() => markTouched('expected')}
            value={fields.expected} onChange={(e) => setField('expected', e.target.value)} />
          {fieldError('expected') && (
            <div id="bug-expected-error" role="alert" style={{ color: 'var(--error)', fontSize: 'var(--font-size-sm)' }}>
              {t('bug.field_required')}
            </div>
          )}
          <label className="input__label" htmlFor="bug-actual">
            {t('bug.actual')}{requiredMark}
          </label>
          <AutoResizeTextarea id="bug-actual" className="input" rows={2}
            aria-required="true"
            aria-invalid={touched.actual && !!errors.actual}
            aria-describedby={fieldError('actual') ? 'bug-actual-error' : undefined}
            placeholder={t('bug.actual_placeholder')}
            onBlur={() => markTouched('actual')}
            value={fields.actual} onChange={(e) => setField('actual', e.target.value)} />
          {fieldError('actual') && (
            <div id="bug-actual-error" role="alert" style={{ color: 'var(--error)', fontSize: 'var(--font-size-sm)' }}>
              {t('bug.field_required')}
            </div>
          )}
          <label className="input__label" htmlFor="bug-extra">
            {t('bug.extra')}{optionalMark}
          </label>
          <AutoResizeTextarea id="bug-extra" className="input" rows={2}
            aria-required="false"
            placeholder={t('bug.extra_placeholder')}
            value={fields.extra} onChange={(e) => setField('extra', e.target.value)} />
        </div>

        <div>
          <div style={{ fontWeight: 600, fontSize: 'var(--font-size-sm)', marginBottom: 'var(--space-sm)' }}>
            {t('bug.diagnostics_heading')}
          </div>
          {preparing ? (
            <div style={{ fontSize: 'var(--font-size-sm)', color: 'var(--text-secondary)' }}>
              <RefreshCw size={13} className="icon-spin" /> {t('bug.collecting')}
            </div>
          ) : (
            <div style={{ display: 'flex', flexDirection: 'column', gap: 6, fontSize: 'var(--font-size-sm)' }}>
              {baseSections.map((section) => (
                <label key={section.id} style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
                  <input type="checkbox"
                    checked={sectionEnabled[section.id] ?? true}
                    onChange={(e) => toggleSection(section.id, e.target.checked)} />
                  {sectionTitle(section.id)}
                </label>
              ))}
              {diagnostics ? (
                <label style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
                  <input type="checkbox"
                    checked={sectionEnabled.diagnostics ?? true}
                    onChange={(e) => toggleSection('diagnostics', e.target.checked)} />
                  {sectionTitle('diagnostics')}
                </label>
              ) : (
                <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                  <Button size="sm" variant="secondary" onClick={runDiagnostics} disabled={diagnosticsLoading || !onRunDiagnostics}>
                    {diagnosticsLoading ? t('diag.running') : t('bug.run_diagnostics')}
                  </Button>
                  <span style={{ color: 'var(--text-tertiary)' }}>{t('bug.diagnostics_optional')}</span>
                </div>
              )}
              <label style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
                <input type="checkbox"
                  checked={sectionEnabled.launcher ?? false}
                  onChange={(e) => {
                    const checked = e.target.checked;
                    toggleSection('launcher', checked);
                    if (checked && !logs.launcher?.content && !logs.launcher?.loading) void loadLog('launcher');
                  }} />
                {sectionTitle('launcher')}
                {logs.launcher?.loading && <RefreshCw size={12} className="icon-spin" />}
              </label>
              {logs.launcher?.error && <div style={{ marginLeft: 24, color: 'var(--text-tertiary)' }}>{logs.launcher.error}</div>}
              <label style={{ display: 'flex', gap: 8, alignItems: 'center', opacity: instanceName ? 1 : 0.5 }}>
                <input type="checkbox" disabled={!instanceName}
                  checked={sectionEnabled.game ?? false}
                  onChange={(e) => {
                    const checked = e.target.checked;
                    toggleSection('game', checked);
                    if (checked && !logs.game?.content && !logs.game?.loading) void loadLog('game');
                  }} />
                {sectionTitle('game')}
                {logs.game?.loading && <RefreshCw size={12} className="icon-spin" />}
              </label>
              {logs.game?.error && <div style={{ marginLeft: 24, color: 'var(--text-tertiary)' }}>{logs.game.error}</div>}
              <label style={{ display: 'flex', gap: 8, alignItems: 'center', opacity: instanceName ? 1 : 0.5 }}>
                <input type="checkbox" disabled={!instanceName}
                  checked={sectionEnabled.crash ?? false}
                  onChange={(e) => {
                    const checked = e.target.checked;
                    toggleSection('crash', checked);
                    if (checked && !logs.crash?.content && !logs.crash?.loading) void loadCrash();
                  }} />
                {sectionTitle('crash')}
                {logs.crash?.loading && <RefreshCw size={12} className="icon-spin" />}
              </label>
              {logs.crash?.error && <div style={{ marginLeft: 24, color: 'var(--text-tertiary)' }}>{logs.crash.error}</div>}
            </div>
          )}
        </div>

        <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-sm)', flexWrap: 'wrap' }}>
          <Button
            size="sm"
            onClick={() => generate()}
            disabled={preparing || !complete}
            title={!complete ? t('bug.required_hint') : undefined}
          >
            {t('bug.generate')}
          </Button>
          <span style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)' }}>
            {!complete ? t('bug.required_hint') : t('bug.preview_hint')}
          </span>
        </div>

        <label className="input__label" htmlFor="bug-preview">{t('bug.preview')}</label>
        <textarea id="bug-preview" className="input" rows={14}
          style={{ fontFamily: 'var(--font-mono, monospace)', resize: 'none' }}
          value={reportText}
          placeholder={t('bug.preview_placeholder')}
          onChange={(e) => {
            setReportText(e.target.value);
            previewEditedRef.current = true;
          }} />
      </div>
    </Modal>
  );
}

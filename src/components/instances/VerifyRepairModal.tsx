import { useCallback, useEffect, useReducer, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { CheckCircle2, AlertTriangle, Wrench } from 'lucide-react';
import { SnakeSpinner } from '../ui/SnakeSpinner';
import { Modal } from '../ui/Modal';
import { Button } from '../ui/Button';
import { ProgressBar } from '../ui/ProgressBar';
import { SlidingTabs } from '../ui/SlidingTabs';
import { useT, type MessageKey } from '../../lib/i18n';
import { RepairResultView } from './RepairResultView';
import {
  initialRepairResultState,
  repairResultReducer,
  type RepairSummary,
} from './repairResult';
import { issueKindLabelKey } from './verifyIssueLabels';

interface VerifyIssue {
  path: string;
  kind: string;
  repairable: boolean;
  /** Extra diagnostic context; content and length are kind-specific. */
  detail?: string | null;
}

interface VerifyCategory {
  id: string;
  checked: number;
  issue_count: number;
  issues: VerifyIssue[];
  verified: number;
  unverified: number;
  fingerprint_only?: boolean;
}

interface VerifyReport {
  categories: VerifyCategory[];
  repairable_count: number;
  content_issues: number;
  healthy: boolean;
  mode: 'quick' | 'deep';
}

interface RepairProgress {
  stage: string;
  percent: number;
  message: string;
}

const CAT_LABELS: Record<string, MessageKey> = {
  metadata: 'verify.cat_metadata',
  minecraft: 'verify.cat_minecraft',
  libraries: 'verify.cat_libraries',
  assets: 'verify.cat_assets',
  loader: 'verify.cat_loader',
  java: 'verify.cat_java',
  content: 'verify.cat_content',
};

/** Which part of an instance to scan: everything, launcher files only, or one
 * content folder. Matches `VerifyScope` in the backend. */
type VerifyScope = 'all' | 'launcher' | 'mods' | 'resourcepacks' | 'shaderpacks';
type VerifyMode = 'quick' | 'deep';

/** In a content-only scope the "content" category shows the folder it scanned. */
const SCOPE_CONTENT_LABELS: Partial<Record<VerifyScope, MessageKey>> = {
  mods: 'verify.scope_mods',
  resourcepacks: 'verify.scope_resourcepacks',
  shaderpacks: 'verify.scope_shaderpacks',
};

interface VerifyRepairModalProps {
  open: boolean;
  onClose: () => void;
  instanceName: string;
}

export function VerifyRepairModal({ open, onClose, instanceName }: VerifyRepairModalProps) {
  const t = useT();
  interface ReportSlot {
    key: string;
    report: VerifyReport;
  }
  const [reportState, setReportState] = useState<ReportSlot | null>(null);
  const [checking, setChecking] = useState(false);
  const [repairing, setRepairing] = useState(false);
  const [scope, setScope] = useState<VerifyScope>('all');
  const [mode, setMode] = useState<VerifyMode>('deep');
  const [progress, setProgress] = useState<RepairProgress | null>(null);
  // Verify and Repair errors are separate: a later Verify must not hide a
  // Repair failure, and the Repair result survives the automatic re-verify.
  const [verifyError, setVerifyError] = useState<string | null>(null);
  const [repairResult, dispatchRepairResult] = useReducer(
    repairResultReducer,
    initialRepairResultState,
  );
  const unlistenRef = useRef<(() => void) | null>(null);
  // `instanceName` is the stable instance/directory identity in this app.
  // Caching is intentionally limited to the open dialog: switching tabs keeps
  // its result, but closing the dialog or changing the instance invalidates
  // every entry. The key also contains the instance so two sessions can never
  // share a report.
  const activeKey = `${instanceName}:${scope}:${mode}`;
  const cacheRef = useRef<Record<string, VerifyReport>>({});
  // Latest scan request wins: a slow reply from a previous instance/scope must
  // never overwrite the current report or its cache entry.
  const requestSeq = useRef(0);
  // These refs track the latest lifecycle values for async callbacks. They are
  // assigned during render so a promise resolution between render and effect
  // cannot observe a stale instance/open flag.
  const openRef = useRef(open);
  openRef.current = open;
  const instanceRef = useRef(instanceName);
  instanceRef.current = instanceName;
  const report = reportState?.key === activeKey ? reportState.report : null;

  const runVerify = useCallback(async (force = false) => {
    dispatchRepairResult({ type: 'verify_started' });
    const requestedInstance = instanceName;
    const requestedKey = `${requestedInstance}:${scope}:${mode}`;
    if (!force) {
      const cached = cacheRef.current[requestedKey];
      if (cached) {
        requestSeq.current += 1;
        setReportState({ key: requestedKey, report: cached });
        setVerifyError(null);
        setChecking(false);
        return;
      }
    } else {
      // A forced scan is a new snapshot, not a confirmation of the previous
      // one. Drop the old successful entry before the request; if the scan
      // fails, returning to this scope must not resurrect a stale Healthy
      // report from cache.
      delete cacheRef.current[requestedKey];
    }
    const seq = ++requestSeq.current;
    setChecking(true);
    setVerifyError(null);
    try {
      const result = await invoke<VerifyReport>('cmd_verify_instance', {
        instanceName: requestedInstance,
        scope,
        mode,
      });
      if (
        seq !== requestSeq.current
        || !openRef.current
        || instanceRef.current !== requestedInstance
      ) {
        return;
      }
      cacheRef.current[requestedKey] = result;
      setReportState({ key: requestedKey, report: result });
    } catch (e) {
      if (
        seq === requestSeq.current
        && openRef.current
        && instanceRef.current === requestedInstance
      ) {
        delete cacheRef.current[requestedKey];
        setVerifyError(String(e));
        setReportState(null);
      }
    } finally {
      if (seq === requestSeq.current) setChecking(false);
    }
  }, [instanceName, scope, mode]);

  useEffect(() => {
    // A different instance must never inherit the previous instance's report
    // or cache, even if this component stays mounted (GameLogs does exactly
    // that). The increment invalidates any request already in flight.
    requestSeq.current += 1;
    cacheRef.current = {};
    setReportState(null);
    setVerifyError(null);
    dispatchRepairResult({ type: 'reset' });
    setProgress(null);
    setChecking(false);
  }, [instanceName]);

  useEffect(() => {
    if (open) {
      runVerify();
    } else {
      // Closing the dialog drops the per-open cache. The next open is a new
      // snapshot, not a replay of a report from before files changed.
      requestSeq.current += 1;
      cacheRef.current = {};
      setReportState(null);
      setVerifyError(null);
      dispatchRepairResult({ type: 'reset' });
      setProgress(null);
      setRepairing(false);
      setChecking(false);
    }
  }, [open, runVerify]);

  // Subscribe to repair_progress for the duration of a repair.
  useEffect(() => {
    if (!repairing) return;
    let cancelled = false;
    listen<RepairProgress>('repair_progress', (event) => {
      if (!cancelled) setProgress(event.payload);
    }).then((un) => {
      if (cancelled) {
        un();
      } else {
        unlistenRef.current = un;
      }
    });
    return () => {
      cancelled = true;
      unlistenRef.current?.();
      unlistenRef.current = null;
    };
  }, [repairing]);

  const runRepair = useCallback(async () => {
    const requestedInstance = instanceName;
    setRepairing(true);
    // The old report belongs to the pre-repair state. Do not leave it next to
    // a failed Repair error or a partial result.
    requestSeq.current += 1;
    cacheRef.current = {};
    setReportState(null);
    setVerifyError(null);
    setChecking(false);
    dispatchRepairResult({ type: 'repair_started' });
    setProgress(null);
    try {
      const result = await invoke<RepairSummary>('cmd_repair_instance', {
        instanceName: requestedInstance,
      });
      if (!openRef.current || instanceRef.current !== requestedInstance) return;
      dispatchRepairResult({ type: 'repair_finished', summary: result });
      // The instance changed on disk — every cached scope is stale now.
      cacheRef.current = {};
      await runVerify(true);
    } catch (e) {
      if (!openRef.current || instanceRef.current !== requestedInstance) return;
      dispatchRepairResult({ type: 'repair_failed', error: String(e) });
    } finally {
      setRepairing(false);
      setProgress(null);
    }
  }, [instanceName, runVerify]);

  const busy = checking || repairing;
  const canRepair = !busy && (report?.repairable_count ?? 0) > 0;
  const unverifiedTotal = report?.categories.reduce((sum, cat) => sum + cat.unverified, 0) ?? 0;

  return (
    <Modal
      open={open}
      onClose={busy ? () => {} : onClose}
      title={t('verify.title')}
      maxWidth={560}
      footer={
        <div style={{ display: 'flex', gap: 'var(--space-md)', justifyContent: 'flex-end', width: '100%' }}>
          <Button variant="ghost" onClick={onClose} disabled={busy}>
            {t('verify.close')}
          </Button>
          {!busy && (
            <Button variant="secondary" onClick={() => runVerify(true)}>
              {t('verify.retry')}
            </Button>
          )}
          <Button variant="primary" onClick={runRepair} disabled={!canRepair} loading={repairing}>
            <Wrench size={14} />
            {repairing ? t('verify.repairing') : t('verify.repair')}
          </Button>
        </div>
      }
    >
      <div style={{ fontSize: 'var(--font-size-sm)', color: 'var(--text-secondary)', marginBottom: 'var(--space-sm)' }}>
        {t('verify.instance_label', { name: instanceName })}
      </div>

      {/* Scope: all / launcher files / one content folder. Changing it
          re-runs the scan (runVerify depends on scope). Repair is always
          launcher-managed, so a content-only scope has nothing to repair.
          Same sliding-frame row as the create dialog's version/loader
          switches (owner directive: the indicator must match those). */}
      <div style={{ marginBottom: 'var(--space-md)' }}>
        <SlidingTabs<VerifyScope>
          ariaLabel={t('verify.scope_label')}
          className="wizard-switch"
          style={{ borderBottom: 'none', marginBottom: 0, justifyContent: 'center' }}
          active={scope}
          onChange={setScope}
          tabs={[
            { id: 'all', label: t('verify.scope_all'), disabled: busy },
            { id: 'launcher', label: t('verify.scope_launcher'), disabled: busy },
            { id: 'mods', label: t('verify.scope_mods'), disabled: busy },
            { id: 'resourcepacks', label: t('verify.scope_resourcepacks'), disabled: busy },
            { id: 'shaderpacks', label: t('verify.scope_shaderpacks'), disabled: busy },
          ]}
        />
      </div>

      {/* Quick is for routine checks; deep is the only mode that proves file
          contents against the checksums and the content sidecars. */}
      <div style={{ marginBottom: 'var(--space-sm)' }}>
        <SlidingTabs<VerifyMode>
          ariaLabel={t('verify.mode_label')}
          className="wizard-switch"
          style={{ borderBottom: 'none', marginBottom: 0, justifyContent: 'center' }}
          active={mode}
          onChange={setMode}
          tabs={[
            { id: 'quick', label: t('verify.mode_quick'), disabled: busy },
            { id: 'deep', label: t('verify.mode_deep'), disabled: busy },
          ]}
        />
        <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)', textAlign: 'center', marginTop: 4 }}>
          {mode === 'quick' ? t('verify.mode_quick_hint') : t('verify.mode_deep_hint')}
        </div>
      </div>

      {checking && (
        <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-md)', padding: 'var(--space-lg) 0', color: 'var(--text-secondary)' }}>
          <SnakeSpinner size={18} />
          <span>{t('verify.checking')}</span>
        </div>
      )}

      {verifyError && !busy && (
        <div style={{ color: 'var(--danger, #e5534b)', fontSize: 'var(--font-size-sm)', marginBottom: 'var(--space-md)' }}>
          {t('verify.error', { error: verifyError })}
        </div>
      )}

      <RepairResultView
        summary={repairResult.summary}
        error={repairResult.error}
      />


      {repairing && progress && (
        <div style={{ marginBottom: 'var(--space-md)' }}>
          <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-secondary)', marginBottom: 'var(--space-xs)' }}>
            {progress.message}
          </div>
          <ProgressBar percent={progress.percent} showLabel />
        </div>
      )}

      {report && !busy && (
        <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--space-sm)' }}>
          {/* Summary line */}
          {report.healthy ? (
            <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-md)', color: unverifiedTotal > 0 ? 'var(--text-secondary)' : 'var(--success, #3fb950)', fontSize: 'var(--font-size-sm)' }}>
              {unverifiedTotal > 0 ? <AlertTriangle size={16} /> : <CheckCircle2 size={16} />}
              <span>
                {unverifiedTotal > 0
                  ? t('verify.ok_with_unverified', { n: unverifiedTotal })
                  : t('verify.ok')}
              </span>
            </div>
          ) : (
            <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--space-xs)', fontSize: 'var(--font-size-sm)' }}>
              {report.repairable_count > 0 && (
                <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-md)', color: 'var(--warning, #d29922)' }}>
                  <AlertTriangle size={16} />
                  <span>{t('verify.repairable', { n: report.repairable_count })}</span>
                </div>
              )}
              {report.content_issues > 0 && (
                <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-md)', color: 'var(--text-secondary)' }}>
                  <AlertTriangle size={16} />
                  <span>{t('verify.content_issues', { n: report.content_issues })}</span>
                </div>
              )}
            </div>
          )}

          {/* Categories */}
          {report.categories.map((cat) => {
            const scopedContent = cat.id === 'content' ? SCOPE_CONTENT_LABELS[scope] : undefined;
            const labelKey = scopedContent ?? CAT_LABELS[cat.id];
            if (!labelKey) return null;
            const hasIssues = cat.issue_count > 0;
            return (
              <div
                key={cat.id}
                style={{
                  display: 'flex',
                  alignItems: 'center',
                  gap: 'var(--space-md)',
                  padding: 'var(--space-sm) var(--space-md)',
                  borderRadius: 'var(--radius-md)',
                  background: hasIssues ? 'var(--surface-glass-hover)' : 'transparent',
                  fontSize: 'var(--font-size-sm)',
                }}
              >
                {hasIssues ? (
                  <AlertTriangle size={14} color="var(--warning, #d29922)" style={{ flexShrink: 0 }} />
                ) : (
                  <CheckCircle2 size={14} color="var(--success, #3fb950)" style={{ flexShrink: 0 }} />
                )}
                <span style={{ flex: 1, fontWeight: hasIssues ? 500 : 400 }}>{t(labelKey)}</span>
                <span style={{ display: 'flex', flexDirection: 'column', alignItems: 'flex-end', gap: 1 }}>
                  {(cat.verified > 0 || cat.unverified > 0) && (
                    <span style={{ color: 'var(--text-tertiary)', fontSize: 'var(--font-size-xs)', whiteSpace: 'nowrap' }}>
                      {t(cat.fingerprint_only ? 'verify.hash_counts_local' : 'verify.hash_counts', {
                        verified: cat.verified.toString(),
                        unverified: cat.unverified.toString(),
                      })}
                    </span>
                  )}
                  <span style={{ color: 'var(--text-tertiary)', fontSize: 'var(--font-size-xs)' }}>
                    {hasIssues
                      ? `${cat.issue_count} / ${cat.checked || cat.issue_count}`
                      : cat.checked > 0
                        ? t('verify.checked_count', { n: cat.checked })
                        : ''}
                  </span>
                </span>
              </div>
            );
          })}

          {/* Issue list (collapsed per category, capped) */}
          {report.categories
            .filter((cat) => cat.issue_count > 0)
            .map((cat) => (
              <div key={`${cat.id}-issues`} style={{ paddingLeft: 'var(--space-xl)', display: 'flex', flexDirection: 'column', gap: 2 }}>
                {cat.issues.slice(0, 20).map((issue, i) => {
                  const labelKey = issueKindLabelKey(issue.kind);
                  return (
                    <div
                      key={i}
                      style={{
                        fontSize: 'var(--font-size-xs)',
                        color: 'var(--text-tertiary)',
                        display: 'flex',
                        flexDirection: 'column',
                        gap: 2,
                        minWidth: 0,
                      }}
                    >
                      <div style={{ display: 'flex', gap: 'var(--space-sm)', alignItems: 'baseline', minWidth: 0 }}>
                        <code style={{ flex: 1, minWidth: 0, overflowWrap: 'anywhere', wordBreak: 'break-word' }}>{issue.path}</code>
                        <span style={{ color: issue.kind === 'missing' ? 'var(--danger, #e5534b)' : 'var(--warning, #d29922)', flexShrink: 0 }}>
                          {labelKey ? t(labelKey) : t('verify.kind_unknown', { kind: issue.kind })}
                        </span>
                      </div>
                      {issue.detail && (
                        <div
                          data-testid="verify-issue-detail"
                          style={{
                            color: 'var(--text-tertiary)',
                            whiteSpace: 'pre-wrap',
                            overflowWrap: 'anywhere',
                            wordBreak: 'break-word',
                          }}
                        >
                          {issue.detail}
                        </div>
                      )}
                    </div>
                  );
                })}
                {cat.issues.length > 20 && (
                  <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)' }}>
                    … +{cat.issues.length - 20}
                  </div>
                )}
              </div>
            ))}

          {report.categories.some((cat) => cat.fingerprint_only && cat.verified > 0) && (
            <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)', marginTop: 'var(--space-sm)' }}>
              {t('verify.local_hash_note')}
            </div>
          )}

          {/* Content note */}
          {report.content_issues > 0 && (
            <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)', marginTop: 'var(--space-sm)', fontStyle: 'italic' }}>
              {t('verify.content_note')}
            </div>
          )}

          {report.repairable_count === 0 && report.content_issues === 0 && !report.healthy && (
            <div style={{ fontSize: 'var(--font-size-sm)', color: 'var(--text-secondary)' }}>
              {t('verify.no_repairable')}
            </div>
          )}
        </div>
      )}
    </Modal>
  );
}

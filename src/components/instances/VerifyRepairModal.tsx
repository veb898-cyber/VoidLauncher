import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { CheckCircle2, AlertTriangle, Loader2, Wrench } from 'lucide-react';
import { Modal } from '../ui/Modal';
import { Button } from '../ui/Button';
import { ProgressBar } from '../ui/ProgressBar';
import { Segmented } from '../ui/Segmented';
import { useT, type MessageKey } from '../../lib/i18n';

interface VerifyIssue {
  path: string;
  kind: string;
  repairable: boolean;
}

interface VerifyCategory {
  id: string;
  checked: number;
  issue_count: number;
  issues: VerifyIssue[];
}

interface VerifyReport {
  categories: VerifyCategory[];
  repairable_count: number;
  content_issues: number;
  healthy: boolean;
}

interface RepairSummary {
  repaired: number;
  failed: number;
  content_skipped: number;
  errors: string[];
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

const KIND_LABELS: Record<string, MessageKey> = {
  missing: 'verify.kind_missing',
  corrupt: 'verify.kind_corrupt',
  invalid: 'verify.kind_invalid',
};

/** Which part of an instance to scan: everything, launcher files only, or one
 * content folder. Matches `VerifyScope` in the backend. */
type VerifyScope = 'all' | 'launcher' | 'mods' | 'resourcepacks' | 'shaderpacks';

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
  const [report, setReport] = useState<VerifyReport | null>(null);
  const [checking, setChecking] = useState(false);
  const [repairing, setRepairing] = useState(false);
  const [scope, setScope] = useState<VerifyScope>('all');
  const [progress, setProgress] = useState<RepairProgress | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [summary, setSummary] = useState<RepairSummary | null>(null);
  const unlistenRef = useRef<(() => void) | null>(null);

  const runVerify = useCallback(async () => {
    setChecking(true);
    setError(null);
    setSummary(null);
    try {
      const result = await invoke<VerifyReport>('cmd_verify_instance', {
        instanceName,
        scope,
      });
      setReport(result);
    } catch (e) {
      setError(String(e));
      setReport(null);
    } finally {
      setChecking(false);
    }
  }, [instanceName, scope]);

  useEffect(() => {
    if (open) {
      runVerify();
    } else {
      setReport(null);
      setError(null);
      setSummary(null);
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
    setRepairing(true);
    setError(null);
    setSummary(null);
    setProgress(null);
    try {
      const result = await invoke<RepairSummary>('cmd_repair_instance', {
        instanceName,
      });
      setSummary(result);
      await runVerify();
    } catch (e) {
      setError(String(e));
    } finally {
      setRepairing(false);
      setProgress(null);
    }
  }, [instanceName, runVerify]);

  const busy = checking || repairing;
  const canRepair = !busy && (report?.repairable_count ?? 0) > 0;

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
            <Button variant="secondary" onClick={runVerify}>
              {t('verify.retry')}
            </Button>
          )}
          <Button variant="primary" onClick={runRepair} disabled={!canRepair} loading={repairing}>
            {repairing ? <Loader2 size={14} className="animate-spin" /> : <Wrench size={14} />}
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
          launcher-managed, so a content-only scope has nothing to repair. */}
      <div style={{ marginBottom: 'var(--space-md)' }}>
        <Segmented
          ariaLabel={t('verify.scope_label')}
          block
          value={scope}
          onChange={setScope}
          options={[
            { value: 'all', label: t('verify.scope_all'), disabled: busy },
            { value: 'launcher', label: t('verify.scope_launcher'), disabled: busy },
            { value: 'mods', label: t('verify.scope_mods'), disabled: busy },
            { value: 'resourcepacks', label: t('verify.scope_resourcepacks'), disabled: busy },
            { value: 'shaderpacks', label: t('verify.scope_shaderpacks'), disabled: busy },
          ]}
        />
      </div>

      {checking && (
        <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-md)', padding: 'var(--space-lg) 0', color: 'var(--text-secondary)' }}>
          <Loader2 size={16} className="animate-spin" />
          <span>{t('verify.checking')}</span>
        </div>
      )}

      {error && !busy && (
        <div style={{ color: 'var(--danger, #e5534b)', fontSize: 'var(--font-size-sm)', marginBottom: 'var(--space-md)' }}>
          {t('verify.error', { error })}
        </div>
      )}

      {summary && !busy && (
        <div style={{ fontSize: 'var(--font-size-sm)', marginBottom: 'var(--space-md)', color: summary.failed > 0 ? 'var(--danger, #e5534b)' : 'var(--success, #3fb950)' }}>
          {t('verify.repaired', { repaired: summary.repaired, failed: summary.failed })}
        </div>
      )}

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
            <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-md)', color: 'var(--success, #3fb950)', fontSize: 'var(--font-size-sm)' }}>
              <CheckCircle2 size={16} />
              <span>{t('verify.ok')}</span>
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
                <span style={{ color: 'var(--text-tertiary)', fontSize: 'var(--font-size-xs)' }}>
                  {hasIssues
                    ? `${cat.issue_count} / ${cat.checked || cat.issue_count}`
                    : cat.checked > 0
                      ? t('verify.checked_count', { n: cat.checked })
                      : ''}
                </span>
              </div>
            );
          })}

          {/* Issue list (collapsed per category, capped) */}
          {report.categories
            .filter((cat) => cat.issue_count > 0)
            .map((cat) => (
              <div key={`${cat.id}-issues`} style={{ paddingLeft: 'var(--space-xl)', display: 'flex', flexDirection: 'column', gap: 2 }}>
                {cat.issues.slice(0, 20).map((issue, i) => (
                  <div
                    key={i}
                    style={{
                      fontSize: 'var(--font-size-xs)',
                      color: 'var(--text-tertiary)',
                      display: 'flex',
                      gap: 'var(--space-sm)',
                      alignItems: 'baseline',
                    }}
                  >
                    <code style={{ flex: 1, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{issue.path}</code>
                    <span style={{ color: issue.kind === 'missing' ? 'var(--danger, #e5534b)' : 'var(--warning, #d29922)', flexShrink: 0 }}>
                      {KIND_LABELS[issue.kind] ? t(KIND_LABELS[issue.kind]) : issue.kind}
                    </span>
                  </div>
                ))}
                {cat.issues.length > 20 && (
                  <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)' }}>
                    … +{cat.issues.length - 20}
                  </div>
                )}
              </div>
            ))}

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

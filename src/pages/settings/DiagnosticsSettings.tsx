import { useCallback, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { AlertTriangle, CheckCircle2, Circle, XCircle } from 'lucide-react';
import { Button } from '../../components/ui/Button';
import { CustomSelect } from '../../components/ui/CustomSelect';
import { useT } from '../../lib/i18n';
import { useInstanceStore } from '../../stores/instanceStore';
import {
  CHECK_LABELS, ISSUE_KEYS, VERIFY_CATEGORY_LABELS,
  type DiagReport,
} from '../../lib/diagnostics';
import { friendlyErrorMessage } from '../../lib/errors';

export function DiagnosticsSettings() {
  const t = useT();
  const selectedInstance = useInstanceStore((s) => s.selectedInstance);
  const instances = useInstanceStore((s) => s.instances);
  const [diagReport, setDiagReport] = useState<DiagReport | null>(null);
  const [diagRunning, setDiagRunning] = useState(false);
  const [diagError, setDiagError] = useState<string | null>(null);
  // Which build the diagnostics checks. Defaults to the build selected in
  // the main window until the user picks another one here — the section used
  // to follow the global selection only, with no way to choose by hand.
  const [diagInstance, setDiagInstance] = useState<string | null>(selectedInstance);
  const [diagInstanceTouched, setDiagInstanceTouched] = useState(false);

  const instanceExists = useCallback(
    (name: string | null): name is string =>
      !!name && instances.some((i) => i.name === name),
    [instances],
  );

  // Follow the global selection until the user picks a build in this
  // section; a hand-picked build stays until it is deleted elsewhere.
  useEffect(() => {
    if (diagInstanceTouched) {
      if (diagInstance && !instanceExists(diagInstance)) {
        setDiagInstanceTouched(false);
        setDiagInstance(instanceExists(selectedInstance) ? selectedInstance : null);
      }
    } else {
      setDiagInstance(instanceExists(selectedInstance) ? selectedInstance : null);
    }
  }, [selectedInstance, instanceExists, diagInstanceTouched, diagInstance]);

  const runDiagnostics = useCallback(async () => {
    setDiagRunning(true);
    setDiagError(null);
    try {
      setDiagReport(await invoke<DiagReport>('cmd_run_diagnostics', {
        instanceName: instanceExists(diagInstance) ? diagInstance : null,
      }));
    } catch (e) {
      setDiagError(friendlyErrorMessage(e, t));
    } finally {
      setDiagRunning(false);
    }
  }, [diagInstance, instanceExists, t]);

  const diagStatusIcon = (status: string) =>
    status === 'ok' ? <CheckCircle2 size={15} color="var(--color-success)" />
    : status === 'warn' ? <AlertTriangle size={15} color="var(--color-warning)" />
    : status === 'error' ? <XCircle size={15} color="var(--color-danger)" />
    : <Circle size={15} color="var(--text-tertiary)" />;

  return (
    <section className="settings-section">
      <h2 className="settings-section__title">{t('settings.section_diagnostics')}</h2>
      <p className="settings-section__desc">{t('diag.desc')}</p>

      <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-md)', marginBottom: 'var(--space-md)', flexWrap: 'wrap' }}>
        <Button onClick={runDiagnostics} disabled={diagRunning}>
          {diagRunning ? t('diag.running') : t('diag.run')}
        </Button>
        <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-sm)', minWidth: 0 }}>
          <span style={{ fontSize: 'var(--font-size-sm)', color: 'var(--text-secondary)', whiteSpace: 'nowrap' }}>
            {t('diag.instance_label')}
          </span>
          <div style={{ width: 260 }}>
            <CustomSelect
              value={diagInstance ?? ''}
              options={[
                ...instances.map((i) => ({
                  value: i.name,
                  label: i.name,
                  description: `${i.mc_version} · ${i.loader}`,
                })),
                { value: '', label: t('diag.instance_system_only') },
              ]}
              onChange={(v) => {
                setDiagInstanceTouched(true);
                setDiagInstance(v || null);
              }}
              disabled={diagRunning}
            />
          </div>
        </div>
      </div>
      {diagError && (
        <div style={{ color: 'var(--color-danger)', fontSize: 'var(--font-size-sm)', marginBottom: 'var(--space-md)' }}>
          {diagError}
        </div>
      )}

      {diagReport && (
        <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--space-lg)' }}>
          {/* System inventory */}
          <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fill, minmax(250px, 1fr))', gap: 'var(--space-sm) var(--space-xl)' }}>
            {diagReport.checks.map((c) => (
              <div key={c.id} style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-sm)', fontSize: 'var(--font-size-sm)', minWidth: 0 }}>
                {diagStatusIcon(c.status)}
                <span style={{ color: 'var(--text-secondary)', whiteSpace: 'nowrap' }}>
                  {CHECK_LABELS[c.id] ? t(CHECK_LABELS[c.id]) : c.id}
                </span>
                <span
                  style={{
                    marginLeft: 'auto', fontWeight: 500, minWidth: 0,
                    overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap',
                    maxWidth: '55%', textAlign: 'right',
                  }}
                  title={c.value}
                >
                  {c.value}
                </span>
              </div>
            ))}
          </div>

          {/* Instance files breakdown (from the offline verify scan) */}
          {diagReport.verify && diagReport.verify.categories.length > 0 && (
            <div>
              <div style={{ fontSize: 'var(--font-size-sm)', fontWeight: 600, marginBottom: 'var(--space-sm)' }}>
                {t('diag.files_breakdown')}
              </div>
              <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fill, minmax(250px, 1fr))', gap: 'var(--space-sm) var(--space-xl)' }}>
                {diagReport.verify.categories.map((cat) => {
                  const ok = cat.issue_count === 0;
                  return (
                    <div key={cat.id} style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-sm)', fontSize: 'var(--font-size-sm)', minWidth: 0 }}>
                      {ok ? <CheckCircle2 size={15} color="var(--color-success)" /> : <XCircle size={15} color="var(--color-danger)" />}
                      <span style={{ color: 'var(--text-secondary)', whiteSpace: 'nowrap' }}>
                        {VERIFY_CATEGORY_LABELS[cat.id] ? t(VERIFY_CATEGORY_LABELS[cat.id]) : cat.id}
                      </span>
                      <span style={{ marginLeft: 'auto', fontWeight: 500, color: ok ? undefined : 'var(--color-danger)' }}>
                        {ok ? t('verify.checked_count', { n: cat.checked }) : String(cat.issue_count)}
                      </span>
                    </div>
                  );
                })}
              </div>
            </div>
          )}

          {/* Detected problems */}
          <div>
            <div style={{ fontSize: 'var(--font-size-sm)', fontWeight: 600, marginBottom: 'var(--space-sm)' }}>
              {t('diag.issues_heading')}
            </div>
            {diagReport.issues.length === 0 ? (
              <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-sm)', fontSize: 'var(--font-size-sm)', color: 'var(--color-success)' }}>
                <CheckCircle2 size={15} />
                <span>{t('diag.no_issues')}</span>
              </div>
            ) : (
              <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--space-sm)' }}>
                {diagReport.issues.map((issue, i) => {
                  const keys = ISSUE_KEYS[issue.id];
                  return (
                    <div key={`${issue.id}-${i}`} style={{ display: 'flex', gap: 'var(--space-sm)', alignItems: 'flex-start' }}>
                      {issue.severity === 'error'
                        ? <XCircle size={15} color="var(--color-danger)" style={{ flexShrink: 0, marginTop: 2 }} />
                        : <AlertTriangle size={15} color="var(--color-warning)" style={{ flexShrink: 0, marginTop: 2 }} />}
                      <div style={{ minWidth: 0 }}>
                        <div style={{ fontSize: 'var(--font-size-sm)', fontWeight: 500 }}>
                          {keys ? t(keys.message, issue.params) : issue.id}
                        </div>
                        {keys && (
                          <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)' }}>
                            {t(keys.fix)}
                          </div>
                        )}
                      </div>
                    </div>
                  );
                })}
              </div>
            )}
          </div>
        </div>
      )}
    </section>
  );
}

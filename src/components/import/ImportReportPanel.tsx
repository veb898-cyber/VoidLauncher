import { AlertTriangle, RefreshCw } from 'lucide-react';
import { Button } from '../ui/Button';
import { useT } from '../../lib/i18n';
import type { ImportReport } from '../../lib/importReport';

interface Props {
  report: ImportReport;
  instanceName?: string;
  retrying?: boolean;
  onRetry?: () => void;
  onDismiss?: () => void;
}

/**
 * Shared "import finished with missing files" panel. A partially imported
 * pack is still usable, so this is a warning with the failed file names,
 * their reasons and a retry button — never a silent success toast.
 */
export function ImportReportPanel({ report, instanceName, retrying = false, onRetry, onDismiss }: Props) {
  const t = useT();
  const retryable = report.failed.filter((f) => f.retryable && f.project_id != null && f.file_id != null);
  const visible = report.failed.slice(0, 12);
  const hidden = report.failed.length - visible.length;

  return (
    <div
      style={{
        marginTop: 'var(--space-md)',
        padding: 'var(--space-md)',
        borderRadius: 'var(--radius-md)',
        border: '1px solid hsla(38, 90%, 55%, 0.45)',
        background: 'hsla(38, 90%, 55%, 0.08)',
        display: 'flex',
        flexDirection: 'column',
        gap: 'var(--space-sm)',
      }}
      role="status"
    >
      <div style={{ display: 'flex', alignItems: 'center', gap: 8, fontWeight: 600 }}>
        <AlertTriangle size={16} color="var(--color-warning, #e6a23c)" />
        <span>{t('import_report.title')}</span>
      </div>

      <div style={{ fontSize: 'var(--font-size-sm)', color: 'var(--text-secondary)' }}>
        {t('import_report.summary', {
          installed: String(report.installed),
          total: String(report.total_files),
        })}
        {instanceName ? ` — ${instanceName}` : ''}
      </div>

      <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)' }}>
        {t('import_report.hint')}
      </div>

      {visible.length > 0 && (
        <ul style={{ margin: 0, paddingLeft: 18, display: 'flex', flexDirection: 'column', gap: 4 }}>
          {visible.map((f, i) => (
            <li key={`${f.file_name}-${i}`} style={{ fontSize: 'var(--font-size-xs)' }}>
              <span style={{ fontWeight: 600 }}>{f.file_name}</span>
              {' — '}
              <span style={{ color: 'var(--text-secondary)' }}>{f.reason}</span>
            </li>
          ))}
        </ul>
      )}
      {hidden > 0 && (
        <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)' }}>
          {t('import_report.more', { n: String(hidden) })}
        </div>
      )}

      {report.skipped > 0 && (
        <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)' }}>
          {t('import_report.skipped', { n: String(report.skipped) })}
        </div>
      )}

      {(retryable.length > 0 || onDismiss) && (
        <div style={{ display: 'flex', gap: 'var(--space-sm)', alignItems: 'center', marginTop: 2 }}>
          {retryable.length > 0 && onRetry && (
            <Button
              size="sm"
              variant="secondary"
              onClick={onRetry}
              loading={retrying}
              disabled={retrying}
            >
              {!retrying && <RefreshCw size={14} style={{ marginRight: 6 }} />}
              {t('import_report.retry', { n: String(retryable.length) })}
            </Button>
          )}
          {onDismiss && (
            <Button size="sm" variant="ghost" onClick={onDismiss} disabled={retrying}>
              {t('import_report.dismiss')}
            </Button>
          )}
        </div>
      )}
    </div>
  );
}

import { AlertTriangle, CheckCircle2 } from 'lucide-react';
import { useT } from '../../lib/i18n';
import type { RepairResultState } from './repairResult';

/**
 * Persistent Repair result. It is intentionally outside the Verify report:
 * the automatic re-verify after Repair updates the report, but never removes
 * the counters or the error details of the operation that just ran.
 */
export function RepairResultView({ summary, error }: RepairResultState) {
  const t = useT();

  if (error) {
    return (
      <div
        role="alert"
        style={{
          color: 'var(--danger, #e5534b)',
          fontSize: 'var(--font-size-sm)',
          marginBottom: 'var(--space-md)',
          whiteSpace: 'pre-wrap',
          overflowWrap: 'anywhere',
        }}
      >
        {t('verify.repair_failed', { error })}
      </div>
    );
  }

  if (!summary) return null;

  const failed = summary.failed > 0;
  return (
    <div
      role={failed ? 'alert' : 'status'}
      data-testid="repair-result"
      style={{
        marginBottom: 'var(--space-md)',
        padding: 'var(--space-sm) var(--space-md)',
        borderRadius: 'var(--radius-md)',
        border: `1px solid ${failed ? 'var(--danger, #e5534b)' : 'var(--success, #3fb950)'}`,
        background: 'var(--surface-glass)',
        color: failed ? 'var(--danger, #e5534b)' : 'var(--success, #3fb950)',
        fontSize: 'var(--font-size-sm)',
      }}
    >
      <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-sm)', fontWeight: 600 }}>
        {failed ? <AlertTriangle size={15} /> : <CheckCircle2 size={15} />}
        <span>
          {failed
            ? t('verify.repair_incomplete', {
                repaired: summary.repaired,
                failed: summary.failed,
              })
            : t('verify.repair_succeeded', { repaired: summary.repaired })}
        </span>
      </div>
      {summary.errors.length > 0 && (
        <div style={{ marginTop: 'var(--space-sm)', color: 'var(--text-secondary)' }}>
          <div style={{ fontWeight: 500, marginBottom: 2 }}>
            {t('verify.repair_error_details')}
          </div>
          <ul style={{ margin: 0, paddingLeft: 'var(--space-lg)' }}>
            {summary.errors.map((message, index) => (
              <li key={`${index}-${message}`} style={{ overflowWrap: 'anywhere' }}>
                {message}
              </li>
            ))}
          </ul>
        </div>
      )}
    </div>
  );
}

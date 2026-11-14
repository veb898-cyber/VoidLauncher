import { useT, type MessageKey } from '../../lib/i18n';
import {
  useJavaDownloadStore,
  type JavaDownloadActive,
} from '../../stores/javaDownloadStore';
import { Modal } from '../ui/Modal';
import { ProgressBar } from '../ui/ProgressBar';
import { SnakeSpinner } from '../ui/SnakeSpinner';

interface Props {
  open: boolean;
  onClose: () => void;
}

function stageLabel(entry: JavaDownloadActive, t: ReturnType<typeof useT>): string {
  if (entry.stage === 'error') return t('launch.java_stage_error');
  return t(`launch.java_stage_${entry.stage}` as MessageKey) || entry.message;
}

/**
 * Launch-time Java setup modal. It renders every active major from the shared
 * store: a Java 17 download must not make Java 21's progress disappear (or
 * vice versa). The store survives remounts, so there is no late `listen()`
 * callback to leak after unmount.
 */
export function JavaSetupModal({ open, onClose }: Props) {
  const t = useT();
  const active = useJavaDownloadStore((s) => s.active);
  const downloads = Object.values(active).sort(
    (a, b) => a.majorVersion - b.majorVersion,
  );

  if (!open) return null;

  return (
    <Modal open={open} onClose={onClose} title={t('launch.java_setup_title')} bare>
      <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--space-md)', minWidth: 340 }}>
        {downloads.length === 0 ? (
          <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-sm)' }}>
            <SnakeSpinner size={18} />
            <span style={{ color: 'var(--text-secondary)', fontSize: 'var(--font-size-sm)' }}>
              {t('launch.checking_java')}
            </span>
          </div>
        ) : (
          downloads.map((entry) => (
            <div
              key={entry.majorVersion}
              style={{ display: 'flex', flexDirection: 'column', gap: 'var(--space-sm)' }}
            >
              <div style={{ display: 'flex', justifyContent: 'space-between', gap: 'var(--space-sm)' }}>
                <span style={{ fontWeight: 600, fontSize: 'var(--font-size-sm)' }}>
                  Java {entry.majorVersion}
                </span>
                <span style={{ color: 'var(--text-tertiary)', fontSize: 'var(--font-size-xs)' }}>
                  {Math.round(entry.percent)}%
                </span>
              </div>
              {entry.stage !== 'error' && (
                <ProgressBar percent={entry.percent} showLabel />
              )}
              <span
                style={{
                  color: entry.stage === 'error' ? 'var(--error)' : 'var(--text-secondary)',
                  fontSize: 'var(--font-size-xs)',
                }}
              >
                {entry.stage === 'error' ? entry.message : stageLabel(entry, t)}
              </span>
              {entry.stage !== 'error' && entry.message && (
                <span style={{ color: 'var(--text-tertiary)', fontSize: 'var(--font-size-xs)' }}>
                  {entry.message}
                </span>
              )}
            </div>
          ))
        )}
        <div style={{ display: 'flex', justifyContent: 'flex-end' }}>
          <button className="btn btn--primary" onClick={onClose}>
            {t('common.close')}
          </button>
        </div>
      </div>
    </Modal>
  );
}

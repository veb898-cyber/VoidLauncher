import { useT } from '../lib/i18n';
import { Modal } from './ui/Modal';
import { ProgressBar } from './ui/ProgressBar';
import { useLogPlaque } from '../lib/uiLog';
import type { UpdaterState } from '../hooks/useUpdater';

interface UpdaterModalProps extends Omit<UpdaterState, 'checking'> {
  onUpdate: () => void;
  onDismiss: () => void;
}

export function UpdaterModal({ updateAvailable, updateInfo, downloading, downloadProgress, installing, error, onUpdate, onDismiss }: UpdaterModalProps) {
  const t = useT();
  useLogPlaque(error ? t('updater.error', { error }) : null, 'error', 'updater');

  if (!updateAvailable && !downloading && !installing) return null;

  return (
    <Modal
      open={updateAvailable || downloading || installing}
      onClose={downloading || installing ? () => {} : onDismiss}
      title={t('updater.title')}
      footer={
        downloading || installing ? undefined : (
          <>
            <button className="btn btn--ghost" onClick={onDismiss}>
              {t('updater.btn_later')}
            </button>
            <button className="btn btn--primary" onClick={onUpdate}>
              {t('updater.btn_update')}
            </button>
          </>
        )
      }
    >
      <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--space-md)' }}>
        {updateInfo && !downloading && !installing && (
          <p style={{ color: 'var(--text-secondary)', margin: 0 }}>
            {t('updater.description')}
          </p>
        )}

        {updateInfo && !downloading && !installing && (
          <p style={{ color: 'var(--text-tertiary)', fontSize: 'var(--font-size-sm)', margin: 0 }}>
            {t('updater.version', { version: updateInfo.version })}
          </p>
        )}

        {downloading && (
          <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--space-sm)' }}>
            <span style={{ color: 'var(--text-secondary)', fontSize: 'var(--font-size-sm)' }}>
              {t('updater.downloading')}
            </span>
            <ProgressBar percent={downloadProgress} showLabel />
          </div>
        )}

        {installing && (
          <span style={{ color: 'var(--text-secondary)', fontSize: 'var(--font-size-sm)' }}>
            {t('updater.installing')}
          </span>
        )}

        {error && (
          <span style={{ color: 'var(--error)', fontSize: 'var(--font-size-sm)' }}>
            {t('updater.error', { error })}
          </span>
        )}
      </div>
    </Modal>
  );
}

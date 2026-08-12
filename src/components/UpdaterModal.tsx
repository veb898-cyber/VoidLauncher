import { useT } from '../lib/i18n';
import { Modal } from './ui/Modal';
import { ProgressBar } from './ui/ProgressBar';
import { useLogPlaque } from '../lib/uiLog';
import type { UpdaterState } from '../hooks/useUpdater';

interface UpdaterModalProps extends Omit<UpdaterState, 'checking'> {
  onUpdate: () => void;
  onDismiss: () => void;
  onRetryCheck: () => void;
  onDismissCheckError: () => void;
}

export function UpdaterModal({ updateAvailable, updateInfo, downloading, downloadProgress, installing, error, checkError, onUpdate, onDismiss, onRetryCheck, onDismissCheckError }: UpdaterModalProps) {
  const t = useT();
  useLogPlaque(error ? t('updater.error', { error }) : null, 'error', 'updater');

  if (checkError) {
    return (
      <Modal
        open
        onClose={onDismissCheckError}
        title={t('updater.check_error_title')}
        footer={
          <>
            <button className="btn btn--ghost" onClick={onDismissCheckError}>
              {t('updater.btn_cancel')}
            </button>
            <button className="btn btn--primary" onClick={onRetryCheck}>
              {t('updater.btn_retry')}
            </button>
          </>
        }
      >
        <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--space-md)' }}>
          <p style={{ color: 'var(--text-secondary)', margin: 0 }}>
            {t('updater.check_error_description')}
          </p>
          <p style={{ color: 'var(--error)', fontSize: 'var(--font-size-sm)', margin: 0 }}>
            {checkError}
          </p>
        </div>
      </Modal>
    );
  }

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

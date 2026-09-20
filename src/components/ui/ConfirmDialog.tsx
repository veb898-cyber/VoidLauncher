import { Modal } from './Modal';
import { Button } from './Button';
import { useT } from '../../lib/i18n';

interface ConfirmDialogProps {
  open: boolean;
  title: string;
  description: string;
  onConfirm: () => void;
  onCancel: () => void;
  confirmLabel?: string;
  cancelLabel?: string;
  busy?: boolean;
}

/** Small confirmation modal with danger confirm button (e.g. destructive actions). */
export function ConfirmDialog({
  open,
  title,
  description,
  onConfirm,
  onCancel,
  confirmLabel,
  cancelLabel,
  busy = false,
}: ConfirmDialogProps) {
  const t = useT();

  return (
    <Modal
      open={open}
      onClose={onCancel}
      title={title}
      maxWidth={440}
      footer={
        <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 'var(--space-sm)' }}>
          <Button variant="ghost" onClick={onCancel} disabled={busy}>
            {cancelLabel ?? t('common.cancel')}
          </Button>
          <Button variant="danger" onClick={onConfirm} loading={busy} disabled={busy}>
            {confirmLabel ?? t('common.confirm')}
          </Button>
        </div>
      }
    >
      <p style={{ margin: 0, color: 'var(--text-secondary)', lineHeight: 1.6 }}>{description}</p>
    </Modal>
  );
}
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
  /**
   * Visual weight of the confirm action. Destructive actions (delete,
   * irreversible cleanup) use `danger`; ordinary/reversible confirmations
   * (leave a room, install, disable a feature) use `primary`.
   */
  variant?: 'danger' | 'primary';
}

/** Small confirmation modal for explicit, user-approved actions. */
export function ConfirmDialog({
  open,
  title,
  description,
  onConfirm,
  onCancel,
  confirmLabel,
  cancelLabel,
  busy = false,
  variant = 'primary',
}: ConfirmDialogProps) {
  const t = useT();

  return (
    <Modal
      open={open}
      onClose={onCancel}
      dismissible={!busy}
      title={title}
      maxWidth={440}
      footer={
        <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 'var(--space-sm)' }}>
          <Button variant="ghost" onClick={onCancel} disabled={busy}>
            {cancelLabel ?? t('common.cancel')}
          </Button>
          <Button variant={variant} onClick={onConfirm} loading={busy} disabled={busy}>
            {confirmLabel ?? t('common.confirm')}
          </Button>
        </div>
      }
    >
      <p style={{ margin: 0, color: 'var(--text-secondary)', lineHeight: 1.6 }}>{description}</p>
    </Modal>
  );
}
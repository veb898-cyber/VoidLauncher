import { Modal } from './Modal';
import { Button } from './Button';
import { AlertTriangle, Check } from 'lucide-react';
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
  /** Visual weight of the confirm button; destructive actions keep `danger`. */
  tone?: 'danger' | 'primary';
}

/** Small confirmation modal; the confirm button is destructive by default. */
export function ConfirmDialog({
  open,
  title,
  description,
  onConfirm,
  onCancel,
  confirmLabel,
  cancelLabel,
  busy = false,
  tone = 'danger',
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
          <Button variant={tone} onClick={onConfirm} loading={busy} disabled={busy}>
            {tone === 'danger' ? <AlertTriangle size={14} /> : <Check size={14} />}
            {confirmLabel ?? t('common.confirm')}
          </Button>
        </div>
      }
    >
      <p style={{ margin: 0, color: 'var(--text-secondary)', lineHeight: 1.6 }}>{description}</p>
    </Modal>
  );
}
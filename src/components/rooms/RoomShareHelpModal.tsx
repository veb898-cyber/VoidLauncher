import { Copy, Check } from 'lucide-react';
import { useState } from 'react';
import { Modal } from '../ui/Modal';
import { Button } from '../ui/Button';
import { useRoomStore } from '../../stores/roomStore';
import { useT } from '../../lib/i18n';
import { copyTextToClipboard } from '../../lib/clipboard';
import { addToast } from '../ui/Toast';

interface RoomShareHelpModalProps {
  open: boolean;
  onClose: () => void;
  code: string | null;
  invitation: string | null;
}

/**
 * One-time "how to invite a friend" instructions: Tailscale Machine Sharing
 * has no public API, so the host creates the invitation link manually in the
 * Tailscale admin console. Explains that step in plain language.
 */
export function RoomShareHelpModal({ open, onClose, code, invitation }: RoomShareHelpModalProps) {
  const t = useT();
  const openAdminConsole = useRoomStore((s) => s.openAdminConsole);
  const [copied, setCopied] = useState(false);

  const copyInvitation = async () => {
    if (!invitation) {
      addToast(t('rooms.invitation_not_ready'), 'error');
      return;
    }
    if (await copyTextToClipboard(invitation)) {
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } else {
      addToast(t('common.copy_failed'), 'error');
    }
  };

  const steps = [
    t('rooms.share_help_step1'),
    t('rooms.share_help_step2'),
    t('rooms.share_help_step3'),
    t('rooms.share_help_step4'),
  ];

  return (
    <Modal open={open} onClose={onClose} title={t('rooms.share_help_title')} maxWidth={520}>
      <p style={{ color: 'var(--text-secondary)', marginBottom: 'var(--space-lg)' }}>
        {t('rooms.share_help_intro')}
      </p>

      <ol
        style={{
          margin: '0 0 var(--space-lg)',
          paddingLeft: 'var(--space-xl)',
          display: 'flex',
          flexDirection: 'column',
          gap: 'var(--space-md)',
          color: 'var(--text-primary)',
          lineHeight: 1.5,
        }}
      >
        {steps.map((step, i) => (
          <li key={i}>{step}</li>
        ))}
      </ol>

      {code && (
        <button
          type="button"
          onClick={copyInvitation}
          aria-label={t('rooms.copy')}
          style={{
            display: 'flex',
            alignItems: 'center',
            justifyContent: 'space-between',
            gap: 'var(--space-md)',
            width: '100%',
            background: 'var(--surface-glass)',
            border: '1px solid var(--surface-border)',
            borderRadius: 'var(--radius-md)',
            padding: 'var(--space-md) var(--space-lg)',
            marginBottom: 'var(--space-sm)',
            color: 'inherit',
            font: 'inherit',
            textAlign: 'left',
            cursor: invitation ? 'pointer' : 'default',
            opacity: invitation ? 1 : 0.6,
          }}
        >
          <span style={{ fontFamily: 'var(--font-mono)', fontSize: 'var(--font-size-lg)', letterSpacing: '2px' }}>
            {code}
          </span>
          {copied ? <Check size={18} color="var(--success)" /> : <Copy size={18} color="var(--text-secondary)" />}
        </button>
      )}

      {code && (
        <p style={{ margin: '0 0 var(--space-lg)', fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)' }}>
          {t('rooms.share_help_copy_note')}
        </p>
      )}

      <p
        style={{
          fontSize: 'var(--font-size-sm)',
          color: 'var(--text-tertiary)',
          marginBottom: 'var(--space-lg)',
          lineHeight: 1.5,
        }}
      >
        {t('rooms.share_help_note')}
      </p>

      <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 'var(--space-sm)' }}>
        <Button variant="ghost" onClick={onClose}>
          {t('rooms.share_help_close')}
        </Button>
        <Button onClick={() => { openAdminConsole(); }}>
          {t('rooms.share_help_open')}
        </Button>
      </div>
    </Modal>
  );
}

import { Copy, Check } from 'lucide-react';
import { useState } from 'react';
import { Modal } from '../ui/Modal';
import { Button } from '../ui/Button';
import { useRoomStore } from '../../stores/roomStore';
import { useT } from '../../lib/i18n';

interface RoomShareHelpModalProps {
  open: boolean;
  onClose: () => void;
  code: string | null;
}

/**
 * One-time "how to invite a friend" instructions: Tailscale Machine Sharing
 * has no public API, so the host creates the invitation link manually in the
 * Tailscale admin console. Explains that step in plain language.
 */
export function RoomShareHelpModal({ open, onClose, code }: RoomShareHelpModalProps) {
  const t = useT();
  const openAdminConsole = useRoomStore((s) => s.openAdminConsole);
  const [copied, setCopied] = useState(false);

  const copyCode = () => {
    if (!code) return;
    navigator.clipboard.writeText(code);
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };

  const steps = [
    t('rooms.share_help_step1'),
    t('rooms.share_help_step2'),
    t('rooms.share_help_step3'),
    t('rooms.share_help_step4', { code: code ?? '—' }),
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
        <div
          onClick={copyCode}
          style={{
            display: 'flex',
            alignItems: 'center',
            justifyContent: 'space-between',
            gap: 'var(--space-md)',
            background: 'var(--surface-glass)',
            border: '1px solid var(--surface-border)',
            borderRadius: 'var(--radius-md)',
            padding: 'var(--space-md) var(--space-lg)',
            marginBottom: 'var(--space-lg)',
            cursor: 'pointer',
          }}
        >
          <span style={{ fontFamily: 'var(--font-mono)', fontSize: 'var(--font-size-lg)', letterSpacing: '2px' }}>
            {code}
          </span>
          {copied ? <Check size={18} color="var(--success)" /> : <Copy size={18} color="var(--text-secondary)" />}
        </div>
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

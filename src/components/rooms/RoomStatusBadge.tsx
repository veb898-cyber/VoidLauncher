import type { RoomStatus } from '../../stores/roomStore';
import { t } from '../../lib/i18n';

/** Compact label showing the current room role (host / guest / none). */
export function RoomStatusBadge({ status }: { status: RoomStatus | null }) {
  if (!status) return null;

  const isActive = status.role !== 'none';
  const label =
    status.role === 'host'
      ? t('rooms.badge_host')
      : status.role === 'guest'
        ? t('rooms.badge_guest')
        : t('rooms.badge_none');

  return (
    <span
      style={{
        display: 'inline-flex',
        alignItems: 'center',
        gap: 'var(--space-sm)',
        padding: '6px 12px',
        borderRadius: 'var(--radius-full)',
        fontSize: 'var(--font-size-sm)',
        fontWeight: 600,
        color: isActive ? 'var(--success)' : 'var(--text-secondary)',
        background: isActive ? 'hsla(150, 80%, 50%, 0.12)' : 'var(--surface-glass)',
        border: `1px solid ${isActive ? 'hsla(150, 80%, 50%, 0.3)' : 'var(--surface-border)'}`,
        whiteSpace: 'nowrap',
      }}
    >
      <span
        style={{
          width: 8,
          height: 8,
          borderRadius: '50%',
          background: isActive ? 'var(--success)' : 'var(--text-tertiary)',
        }}
      />
      {label}
    </span>
  );
}

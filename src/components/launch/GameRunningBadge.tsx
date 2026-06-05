import { Gamepad2 } from 'lucide-react';
import { useEventStore } from '../../hooks/useGameEvents';

export function GameRunningBadge() {
  const runningGameId = useEventStore((s) => s.runningGameId);

  if (!runningGameId) return null;

  return (
    <div
      style={{
        display: 'flex',
        alignItems: 'center',
        gap: 'var(--space-sm)',
        padding: 'var(--space-sm) var(--space-md)',
        background: 'hsla(150, 80%, 50%, 0.1)',
        border: '1px solid hsla(150, 80%, 50%, 0.2)',
        borderRadius: 'var(--radius-md)',
        fontSize: 'var(--font-size-xs)',
        color: 'var(--success)',
        margin: 'var(--space-sm)',
        minWidth: 0,
        maxWidth: '100%',
        overflow: 'hidden',
      }}
    >
      <Gamepad2 size={14} style={{ flexShrink: 0 }} />
      <span style={{
        overflow: 'hidden',
        textOverflow: 'ellipsis',
        whiteSpace: 'nowrap',
        minWidth: 0,
      }}>
        {runningGameId}
      </span>
    </div>
  );
}

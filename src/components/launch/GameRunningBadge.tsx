import { Gamepad2 } from 'lucide-react';
import { useEventStore } from '../../hooks/useGameEvents';

export function GameRunningBadge() {
  const runningGameId = useEventStore((s) => s.runningGameId);

  if (!runningGameId) return null;

  return (
    <div
      title={runningGameId}
      aria-label={runningGameId}
      style={{
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'center',
        padding: 'var(--space-sm)',
        background: 'hsla(150, 80%, 50%, 0.1)',
        border: '1px solid hsla(150, 80%, 50%, 0.2)',
        borderRadius: 'var(--radius-md)',
        color: 'var(--success)',
        margin: 'var(--space-sm)',
        width: '36px',
        height: '36px',
        flexShrink: 0,
      }}
    >
      <Gamepad2 size={18} style={{ flexShrink: 0 }} />
    </div>
  );
}

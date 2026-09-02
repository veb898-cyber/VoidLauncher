import { Gamepad2 } from 'lucide-react';
import { useEventStore } from '../../hooks/useGameEvents';
import { Tooltip } from '../ui/Tooltip';

export function GameRunningBadge() {
  const runningGameIds = useEventStore((s) => s.runningGameIds);

  if (runningGameIds.length === 0) return null;

  const single = runningGameIds.length === 1;
  const label = single ? runningGameIds[0] : `${runningGameIds.length} games running`;

  return (
    <Tooltip content={runningGameIds.join('\n')}>
      <div
        aria-label={label}
        style={{
          display: 'flex',
          alignItems: 'center',
          justifyContent: 'center',
          gap: 'var(--space-xs)',
          padding: 'var(--space-sm)',
          background: 'hsla(150, 80%, 50%, 0.1)',
          border: '1px solid hsla(150, 80%, 50%, 0.2)',
          borderRadius: 'var(--radius-md)',
          color: 'var(--success)',
          margin: 'var(--space-sm)',
          minWidth: 36,
          height: 36,
          flexShrink: 0,
          fontSize: 'var(--font-size-xs)',
        }}
      >
        <Gamepad2 size={18} style={{ flexShrink: 0 }} />
        {!single && <span>{runningGameIds.length}</span>}
      </div>
    </Tooltip>
  );
}

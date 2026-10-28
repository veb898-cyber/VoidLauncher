import { useEffect, useState } from 'react';
import { Gamepad2, Square } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import { useEventStore } from '../../hooks/useGameEvents';
import { Tooltip } from '../ui/Tooltip';
import { Modal } from '../ui/Modal';
import { Button } from '../ui/Button';
import { useT } from '../../lib/i18n';
import { useLogStore } from '../../stores/logStore';

export function GameRunningBadge() {
  const t = useT();
  const addLog = useLogStore((s) => s.addLog);
  const runningGameIds = useEventStore((s) => s.runningGameIds);
  const [open, setOpen] = useState(false);
  const [stopping, setStopping] = useState<string | null>(null);

  // The list can empty out on its own (the game exits or is stopped while
  // the dialog is open) — close the dialog so it never shows an empty shell.
  useEffect(() => {
    if (open && runningGameIds.length === 0) setOpen(false);
  }, [open, runningGameIds.length]);

  if (runningGameIds.length === 0) return null;

  const single = runningGameIds.length === 1;
  const label = single ? runningGameIds[0] : t('launch.running_games');

  const stop = async (id: string) => {
    setStopping(id);
    try {
      await invoke('cmd_stop_game', { instanceName: id });
    } catch (e) {
      addLog({ level: 'error', source: 'launch', message: String(e) });
    } finally {
      setStopping(null);
    }
  };

  return (
    <>
      <Tooltip content={runningGameIds.join('\n')}>
        <button
          type="button"
          aria-label={label}
          onClick={() => setOpen(true)}
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
            cursor: 'pointer',
            transition: 'background 200ms cubic-bezier(0.33, 1, 0.68, 1)',
          }}
        >
          <Gamepad2 size={18} style={{ flexShrink: 0 }} />
          {!single && <span>{runningGameIds.length}</span>}
        </button>
      </Tooltip>

      <Modal
        open={open}
        onClose={() => setOpen(false)}
        title={t('launch.running_games')}
        maxWidth={440}
      >
        <p
          style={{
            margin: '0 0 var(--space-md)',
            color: 'var(--text-secondary)',
            lineHeight: 1.6,
          }}
        >
          {t('launch.stop_game_hint')}
        </p>
        <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--space-sm)' }}>
          {runningGameIds.map((id) => (
            <div
              key={id}
              style={{
                display: 'flex',
                alignItems: 'center',
                justifyContent: 'space-between',
                gap: 'var(--space-md)',
                padding: 'var(--space-sm) var(--space-md)',
                background: 'var(--bg-tertiary)',
                borderRadius: 'var(--radius-md)',
              }}
            >
              <span
                title={id}
                style={{
                  overflow: 'hidden',
                  textOverflow: 'ellipsis',
                  whiteSpace: 'nowrap',
                  minWidth: 0,
                }}
              >
                {id}
              </span>
              <Button
                variant="danger"
                size="sm"
                loading={stopping === id}
                disabled={stopping !== null && stopping !== id}
                onClick={() => stop(id)}
              >
                <Square size={13} />
                {t('launch.stop_game')}
              </Button>
            </div>
          ))}
        </div>
      </Modal>
    </>
  );
}

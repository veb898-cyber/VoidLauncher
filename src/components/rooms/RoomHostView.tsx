import { useEffect, useRef, useState } from 'react';
import { Copy, Check, Play, Radio, ExternalLink, LogOut, Users } from 'lucide-react';
import { Button } from '../ui/Button';
import { RefreshIcon } from '../ui/RefreshIcon';
import { CustomSelect } from '../ui/CustomSelect';
import type { SelectOption } from '../ui/CustomSelect';
import { RoomShareHelpModal } from './RoomShareHelpModal';
import { ConfirmDialog } from '../ui/ConfirmDialog';
import { useRoomStore } from '../../stores/roomStore';
import { useInstanceStore } from '../../stores/instanceStore';
import type { Instance } from '../../stores/instanceStore';
import type { RoomStatus } from '../../stores/roomStore';
import { useT } from '../../lib/i18n';
import { copyTextToClipboard } from '../../lib/clipboard';
import { addToast } from '../ui/Toast';
import { resolveHostInstanceSelection } from './hostInstanceSelection';

interface RoomHostViewProps {
  status: RoomStatus;
  instances: Instance[];
  /** False while the instance list is still being fetched on first load. */
  instancesLoaded: boolean;
}

/** Host side: share the room code, launch the world and advertise its LAN port. */
export function RoomHostView({ status, instances, instancesLoaded }: RoomHostViewProps) {
  const t = useT();
  const leave = useRoomStore((s) => s.leave);
  const busy = useRoomStore((s) => s.busy);
  const invitation = useRoomStore((s) => s.invitation);
  const loadInvitation = useRoomStore((s) => s.loadInvitation);
  const detectMinecraftPort = useRoomStore((s) => s.detectMinecraftPort);
  const setHostInstance = useRoomStore((s) => s.setHostInstance);
  const openAdminConsole = useRoomStore((s) => s.openAdminConsole);
  const launchGame = useInstanceStore((s) => s.launchGame);

  const [selected, setSelected] = useState<string>('');
  const [missingSavedInstance, setMissingSavedInstance] = useState(false);
  const [copied, setCopied] = useState(false);
  const [showHelp, setShowHelp] = useState(false);
  const [confirmLeave, setConfirmLeave] = useState(false);
  const [detectingPort, setDetectingPort] = useState(false);
  // An explicit choice stays visible until the backend status confirms it;
  // otherwise a status poll carrying the previous binding would snap back.
  const pendingSelectionRef = useRef<string | null>(null);

  // Restore the registered instance from the backend. This effect only
  // changes the local select value: it never registers an instance, so simply
  // opening the page cannot clear the active room's LAN port.
  useEffect(() => {
    const result = resolveHostInstanceSelection({
      savedInstance: status.hostInstance ?? null,
      instanceNames: instances.map((instance) => instance.name),
      instancesLoaded,
      currentSelection: selected,
      pendingSelection: pendingSelectionRef.current,
    });
    pendingSelectionRef.current = result.pendingSelection;
    setMissingSavedInstance(result.missingSavedInstance);
    if (result.selection !== selected) setSelected(result.selection);
  }, [status.hostInstance, instances, instancesLoaded, selected]);

  // Explicit user choice: register exactly this instance. The backend clears
  // the previous port and starts the monitor for the new game.
  const selectHostInstance = async (name: string) => {
    if (!name || name === selected) return;
    const previous = selected;
    pendingSelectionRef.current = name;
    setSelected(name);
    const saved = await setHostInstance(name);
    if (!saved && pendingSelectionRef.current === name) {
      pendingSelectionRef.current = null;
      setSelected(previous);
    }
  };

  const launchSelected = async () => {
    if (!selected) return;
    if (status.hostInstance !== selected) {
      pendingSelectionRef.current = selected;
      const saved = await setHostInstance(selected);
      if (!saved) {
        pendingSelectionRef.current = null;
        return;
      }
    }
    launchGame(selected);
  };

  const detectSelectedPort = async () => {
    if (!selected) return;
    setDetectingPort(true);
    try {
      if (status.hostInstance !== selected) {
        pendingSelectionRef.current = selected;
        const saved = await setHostInstance(selected);
        if (!saved) {
          pendingSelectionRef.current = null;
          return;
        }
      }
      await detectMinecraftPort(selected);
    } finally {
      setDetectingPort(false);
    }
  };

  // The invitation is fetched separately so `cmd_room_status` (polled every
  // few seconds) never carries the room secret.
  useEffect(() => {
    if (status.role === 'host') void loadInvitation();
  }, [status.role, status.roomId, loadInvitation]);

  const options: SelectOption<string>[] = instances.map((i) => ({
    value: i.name,
    label: i.name,
    description: `${i.mc_version} · ${i.loader}`,
  }));

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

  const portReady = !!status.minecraftPort && !!status.endpoint;

  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--space-xl)', width: '100%' }}>
      <div className="glass-card animate-slide-up">
        <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'flex-start', gap: 'var(--space-md)', flexWrap: 'wrap' }}>
          <div>
            <h2 style={{ margin: '0 0 var(--space-xs)', fontSize: 'var(--font-size-xl)' }}>{t('rooms.host_title')}</h2>
            <p style={{ margin: 0, color: 'var(--text-secondary)' }}>
              {status.roomName ? status.roomName : t('rooms.host_code_label')}
            </p>
          </div>
          <Button variant="ghost" onClick={() => setShowHelp(true)}>
            <Users size={16} style={{ marginRight: 6 }} />
            {t('rooms.host_share_help')}
          </Button>
        </div>

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
            padding: 'var(--space-lg) var(--space-xl)',
            marginTop: 'var(--space-lg)',
            color: 'inherit',
            font: 'inherit',
            textAlign: 'left',
            cursor: 'pointer',
            transition: 'border-color 200ms ease-out, background 200ms ease-out',
          }}
        >
          <div>
            <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)', marginBottom: 2 }}>
              {t('rooms.host_code_label')}
            </div>
            <div style={{ fontSize: 'var(--font-size-2xl)', fontWeight: 800, letterSpacing: '4px', fontFamily: 'var(--font-mono)', color: 'var(--text-primary)' }}>
              {status.roomId ?? '—'}
            </div>
          </div>
          {copied ? <Check size={22} color="var(--success)" /> : <Copy size={22} color="var(--text-secondary)" />}
        </button>

        <p style={{ margin: 'var(--space-md) 0 0', fontSize: 'var(--font-size-sm)', color: 'var(--text-tertiary)' }}>
          {copied ? t('rooms.copied') : t('rooms.host_copy_hint')}
        </p>
      </div>

      <div className="glass-card animate-slide-up">
        <h3 style={{ margin: '0 0 var(--space-md)', fontSize: 'var(--font-size-lg)' }}>{t('rooms.host_launch_step')}</h3>

        <div style={{ display: 'flex', gap: 'var(--space-md)', alignItems: 'flex-end', flexWrap: 'wrap' }}>
          <div style={{ flex: 1, minWidth: 220 }}>
            <label style={{ display: 'block', fontSize: 'var(--font-size-sm)', color: 'var(--text-secondary)', marginBottom: 'var(--space-xs)' }}>
              {t('rooms.host_instance_label')}
            </label>
            {options.length > 0 ? (
              <CustomSelect<string>
                value={selected}
                options={options}
                onChange={selectHostInstance}
                ariaLabel={t('rooms.host_instance_label')}
              />
            ) : instancesLoaded ? (
              <p style={{ color: 'var(--text-tertiary)', margin: 0 }}>{t('rooms.no_instances')}</p>
            ) : (
              <p style={{ color: 'var(--text-tertiary)', margin: 0 }}>{t('rooms.instances_loading')}</p>
            )}
            {missingSavedInstance && (
              <p style={{ margin: 'var(--space-xs) 0 0', color: 'var(--color-warning)', fontSize: 'var(--font-size-sm)', lineHeight: 1.45 }}>
                {t('rooms.host_instance_missing')}
              </p>
            )}
          </div>
          <Button
            onClick={launchSelected}
            disabled={!selected}
          >
            <Play size={16} style={{ marginRight: 6 }} />
            {t('rooms.host_launch')}
          </Button>
        </div>

        <div style={{ display: 'flex', gap: 'var(--space-md)', alignItems: 'center', marginTop: 'var(--space-lg)', flexWrap: 'wrap' }}>
          <Button
            variant="secondary"
            disabled={!selected || detectingPort}
            onClick={detectSelectedPort}
          >
            <RefreshIcon spinning={detectingPort} size={16} />
            {t('rooms.host_detect')}
          </Button>
          <span style={{ color: portReady ? 'var(--success)' : 'var(--text-tertiary)', fontSize: 'var(--font-size-sm)' }}>
            {portReady
              ? t('rooms.host_port_found', { port: String(status.minecraftPort) })
              : t('rooms.host_waiting_port')}
          </span>
        </div>

        {portReady && (
          <div
            style={{
              marginTop: 'var(--space-lg)',
              background: 'var(--surface-glass)',
              border: '1px solid var(--surface-border)',
              borderRadius: 'var(--radius-md)',
              padding: 'var(--space-md) var(--space-lg)',
            }}
          >
            <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)', marginBottom: 2 }}>
              {t('rooms.host_endpoint_label')}
            </div>
            <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-sm)', fontFamily: 'var(--font-mono)' }}>
              <Radio size={16} color="var(--success)" />
              {status.endpoint}
            </div>
            <p style={{ margin: 'var(--space-sm) 0 0', color: 'var(--success)', fontSize: 'var(--font-size-sm)' }}>
              {t('rooms.host_ready')}
            </p>
          </div>
        )}
      </div>

      <div className="glass-card animate-slide-up">
        <h3 style={{ margin: '0 0 var(--space-xs)', fontSize: 'var(--font-size-lg)' }}>{t('rooms.host_revoke_title')}</h3>
        <p style={{ margin: '0 0 var(--space-md)', color: 'var(--text-secondary)' }}>{t('rooms.host_revoke_desc')}</p>
        <Button variant="secondary" onClick={() => { openAdminConsole(); }}>
          <ExternalLink size={16} style={{ marginRight: 6 }} />
          {t('rooms.host_revoke_btn')}
        </Button>
      </div>

      <div>
        <Button variant="danger" onClick={() => { setConfirmLeave(true); }} disabled={busy}>
          <LogOut size={16} style={{ marginRight: 6 }} />
          {t('rooms.leave')}
        </Button>
      </div>

      <ConfirmDialog
        open={confirmLeave}
        title={t('rooms.leave_confirm_title')}
        description={t('rooms.leave_confirm_host_desc')}
        confirmLabel={t('rooms.leave')}
        variant="primary"
        busy={busy}
        onCancel={() => setConfirmLeave(false)}
        onConfirm={() => { leave(); }}
      />

      <RoomShareHelpModal
        open={showHelp}
        onClose={() => setShowHelp(false)}
        code={status.roomId}
        invitation={invitation}
      />
    </div>
  );
}

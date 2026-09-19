import { useEffect, useRef, useState } from 'react';
import { Copy, Check, Play, Radio, RefreshCw, ExternalLink, LogOut, Users } from 'lucide-react';
import { Button } from '../ui/Button';
import { CustomSelect } from '../ui/CustomSelect';
import type { SelectOption } from '../ui/CustomSelect';
import { RoomShareHelpModal } from './RoomShareHelpModal';
import { useRoomStore } from '../../stores/roomStore';
import { useInstanceStore } from '../../stores/instanceStore';
import type { Instance } from '../../stores/instanceStore';
import { useEventStore } from '../../hooks/useGameEvents';
import type { RoomStatus } from '../../stores/roomStore';
import { useT } from '../../lib/i18n';

interface RoomHostViewProps {
  status: RoomStatus;
  instances: Instance[];
}

/** Host side: share the room code, launch the world and advertise its LAN port. */
export function RoomHostView({ status, instances }: RoomHostViewProps) {
  const t = useT();
  const leave = useRoomStore((s) => s.leave);
  const busy = useRoomStore((s) => s.busy);
  const detectMinecraftPort = useRoomStore((s) => s.detectMinecraftPort);
  const reportMinecraftPort = useRoomStore((s) => s.reportMinecraftPort);
  const openAdminConsole = useRoomStore((s) => s.openAdminConsole);
  const launchGame = useInstanceStore((s) => s.launchGame);
  const runningIds = useEventStore((s) => s.runningGameIds);

  const [selected, setSelected] = useState<string>(instances[0]?.name ?? '');
  const [copied, setCopied] = useState(false);
  const [showHelp, setShowHelp] = useState(false);

  // Instances this component has positively seen running (fed by the
  // game_started/launch_complete events in useGameEvents). Used to distinguish
  // "game exited" from "launch state not seeded yet" during app startup.
  const seenRunningRef = useRef<Set<string>>(new Set());

  useEffect(() => {
    if (!selected && instances.length > 0) setSelected(instances[0].name);
  }, [instances, selected]);

  // While the host's game is running but the LAN port is not known yet, scan
  // the live game log periodically until "Open to LAN" prints the port.
  useEffect(() => {
    if (status.minecraftPort) return;
    if (!selected || !runningIds.includes(selected)) return;
    const id = window.setInterval(() => { detectMinecraftPort(); }, 4000);
    return () => window.clearInterval(id);
  }, [status.minecraftPort, selected, runningIds, detectMinecraftPort]);

  // Stop advertising the LAN port once the world the host shared has exited:
  // `launch_complete` drops the instance from runningGameIds, and a stale
  // port would keep the guest's "Join world" live against a closed world.
  // Only fires after this component positively saw the instance running, so a
  // still-live world is never cleared (e.g. during the async launch-state
  // seed right after the app starts).
  useEffect(() => {
    for (const id of runningIds) seenRunningRef.current.add(id);
    if (
      status.minecraftPort &&
      !runningIds.includes(selected) &&
      seenRunningRef.current.has(selected)
    ) {
      seenRunningRef.current.delete(selected);
      reportMinecraftPort(null);
    }
  }, [selected, runningIds, status.minecraftPort, reportMinecraftPort]);

  const options: SelectOption<string>[] = instances.map((i) => ({
    value: i.name,
    label: i.name,
    description: `${i.mc_version} · ${i.loader}`,
  }));

  const copyCode = () => {
    if (!status.roomId) return;
    navigator.clipboard.writeText(status.roomId);
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };

  const portReady = !!status.minecraftPort && !!status.endpoint;

  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--space-xl)', maxWidth: 860 }}>
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

        <div
          onClick={copyCode}
          style={{
            display: 'flex',
            alignItems: 'center',
            justifyContent: 'space-between',
            gap: 'var(--space-md)',
            background: 'var(--gradient-primary)',
            borderRadius: 'var(--radius-lg)',
            padding: 'var(--space-lg) var(--space-xl)',
            marginTop: 'var(--space-lg)',
            cursor: 'pointer',
            color: 'white',
          }}
        >
          <div>
            <div style={{ fontSize: 'var(--font-size-xs)', opacity: 0.8, marginBottom: 2 }}>{t('rooms.host_code_label')}</div>
            <div style={{ fontSize: 'var(--font-size-2xl)', fontWeight: 800, letterSpacing: '4px', fontFamily: 'var(--font-mono)' }}>
              {status.roomId ?? '—'}
            </div>
          </div>
          {copied ? <Check size={22} /> : <Copy size={22} />}
        </div>

        <p style={{ margin: 'var(--space-md) 0 0', fontSize: 'var(--font-size-sm)', color: 'var(--text-tertiary)' }}>
          {copied ? t('rooms.copied') : t('rooms.copy')}
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
              <CustomSelect<string> value={selected} options={options} onChange={setSelected} />
            ) : (
              <p style={{ color: 'var(--text-tertiary)', margin: 0 }}>{t('rooms.no_instances')}</p>
            )}
          </div>
          <Button
            onClick={() => { if (selected) launchGame(selected); }}
            disabled={!selected}
          >
            <Play size={16} style={{ marginRight: 6 }} />
            {t('rooms.host_launch')}
          </Button>
        </div>

        <div style={{ display: 'flex', gap: 'var(--space-md)', alignItems: 'center', marginTop: 'var(--space-lg)', flexWrap: 'wrap' }}>
          <Button variant="secondary" onClick={() => { detectMinecraftPort(); }}>
            <RefreshCw size={16} style={{ marginRight: 6 }} />
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
        <Button variant="ghost" onClick={() => { leave(); }} loading={busy} disabled={busy}>
          <LogOut size={16} style={{ marginRight: 6 }} />
          {t('rooms.leave')}
        </Button>
      </div>

      <RoomShareHelpModal open={showHelp} onClose={() => setShowHelp(false)} code={status.roomId} />
    </div>
  );
}

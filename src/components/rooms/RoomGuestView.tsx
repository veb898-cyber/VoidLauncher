import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Copy, Check, Play, RefreshCw, LogOut, Globe, WifiOff } from 'lucide-react';
import { Button } from '../ui/Button';
import { CustomSelect } from '../ui/CustomSelect';
import type { SelectOption } from '../ui/CustomSelect';
import { LoadingSpinner } from '../ui/LoadingSpinner';
import { useRoomStore } from '../../stores/roomStore';
import type { RoomStatus } from '../../stores/roomStore';
import { useInstanceStore } from '../../stores/instanceStore';
import type { Instance } from '../../stores/instanceStore';
import { useT } from '../../lib/i18n';

interface RoomGuestViewProps {
  status: RoomStatus;
  instances: Instance[];
}

/** Strip the trailing dot Tailscale appends to MagicDNS names. */
function hostAddress(status: RoomStatus): string {
  const host = status.host;
  if (!host) return '';
  const dns = host.dns_name.replace(/\.$/, '');
  return dns || host.tailnet_ips[0] || '';
}

/** Guest side: find the host, then join the world it advertises. */
export function RoomGuestView({ status, instances }: RoomGuestViewProps) {
  const t = useT();
  const refresh = useRoomStore((s) => s.refresh);
  const leave = useRoomStore((s) => s.leave);
  const busy = useRoomStore((s) => s.busy);
  const launchGame = useInstanceStore((s) => s.launchGame);

  const [selected, setSelected] = useState<string>(instances[0]?.name ?? '');
  const [autoJoin, setAutoJoin] = useState<boolean | null>(null);
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    if (!selected && instances.length > 0) setSelected(instances[0].name);
  }, [instances, selected]);

  const addr = hostAddress(status);
  const port = status.minecraftPort;
  const hostReady = !!status.host && status.hostOnline;
  const worldReady = hostReady && !!addr && !!port;

  // Ask the backend whether this Minecraft version supports the legacy
  // `--server/--port` auto-join. 1.19+ needs Direct Connect instead.
  useEffect(() => {
    let alive = true;
    if (!selected || !addr || !port) {
      setAutoJoin(null);
      return;
    }
    const mcVersion = instances.find((i) => i.name === selected)?.mc_version ?? '';
    invoke<string[] | null>('cmd_room_join_args', { mcVersion, host: addr, port })
      .then((args) => { if (alive) setAutoJoin(!!args && args.length > 0); })
      .catch(() => { if (alive) setAutoJoin(false); });
    return () => { alive = false; };
  }, [selected, addr, port, instances]);

  const options: SelectOption<string>[] = instances.map((i) => ({
    value: i.name,
    label: i.name,
    description: `${i.mc_version} · ${i.loader}`,
  }));

  const copyEndpoint = () => {
    if (!status.endpoint) return;
    navigator.clipboard.writeText(status.endpoint);
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };

  const handleJoin = () => {
    if (!selected || !addr || !port) return;
    if (autoJoin) {
      launchGame(selected, addr, port);
    } else {
      launchGame(selected);
    }
  };

  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--space-xl)', maxWidth: 860 }}>
      <div className="glass-card animate-slide-up">
        <h2 style={{ margin: '0 0 var(--space-sm)', fontSize: 'var(--font-size-xl)' }}>
          {t('rooms.guest_title', { code: status.roomId ?? '—' })}
        </h2>

        <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-sm)', marginTop: 'var(--space-md)' }}>
          {!hostReady && <LoadingSpinner size={16} />}
          {hostReady && <Globe size={18} color="var(--success)" />}
          <span style={{ color: hostReady ? 'var(--success)' : 'var(--text-secondary)' }}>
            {!status.host && t('rooms.guest_searching')}
            {status.host && !status.hostOnline && t('rooms.guest_host_offline')}
            {status.host && status.hostOnline && t('rooms.guest_host_online', { name: status.host.host_name || status.host.dns_name })}
          </span>
        </div>

        {hostReady && !worldReady && (
          <p style={{ margin: 'var(--space-sm) 0 0', color: 'var(--text-tertiary)' }}>
            {t('rooms.guest_waiting_port')}
          </p>
        )}
        {!hostReady && (
          <p style={{ margin: 'var(--space-sm) 0 0', color: 'var(--text-tertiary)' }}>
            {t('rooms.guest_leave_hint')}
          </p>
        )}
      </div>

      {worldReady && (
        <div className="glass-card animate-slide-up">
          <h3 style={{ margin: '0 0 var(--space-md)', fontSize: 'var(--font-size-lg)' }}>{t('rooms.guest_ready')}</h3>

          <div style={{ display: 'flex', gap: 'var(--space-md)', alignItems: 'flex-end', flexWrap: 'wrap' }}>
            <div style={{ flex: 1, minWidth: 220 }}>
              <label style={{ display: 'block', fontSize: 'var(--font-size-sm)', color: 'var(--text-secondary)', marginBottom: 'var(--space-xs)' }}>
                {t('rooms.guest_instance_label')}
              </label>
              {options.length > 0 ? (
                <CustomSelect<string> value={selected} options={options} onChange={setSelected} />
              ) : (
                <p style={{ color: 'var(--text-tertiary)', margin: 0 }}>{t('rooms.no_instances')}</p>
              )}
            </div>
            <Button onClick={handleJoin} disabled={!selected} loading={autoJoin === null}>
              <Play size={16} style={{ marginRight: 6 }} />
              {autoJoin ? t('rooms.guest_join') : t('rooms.guest_launch_manual')}
            </Button>
          </div>

          <div
            onClick={copyEndpoint}
            style={{
              display: 'flex',
              alignItems: 'center',
              justifyContent: 'space-between',
              gap: 'var(--space-md)',
              background: 'var(--surface-glass)',
              border: '1px solid var(--surface-border)',
              borderRadius: 'var(--radius-md)',
              padding: 'var(--space-md) var(--space-lg)',
              marginTop: 'var(--space-lg)',
              cursor: 'pointer',
            }}
          >
            <div>
              <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)', marginBottom: 2 }}>
                {t('rooms.guest_endpoint_label')}
              </div>
              <div style={{ fontFamily: 'var(--font-mono)' }}>{status.endpoint}</div>
            </div>
            {copied ? <Check size={18} color="var(--success)" /> : <Copy size={18} color="var(--text-secondary)" />}
          </div>

          {!autoJoin && autoJoin !== null && (
            <div
              style={{
                marginTop: 'var(--space-lg)',
                padding: 'var(--space-md) var(--space-lg)',
                borderRadius: 'var(--radius-md)',
                background: 'var(--surface-glass)',
                border: '1px solid var(--surface-border)',
              }}
            >
              <div style={{ fontWeight: 600, marginBottom: 'var(--space-xs)' }}>{t('rooms.guest_manual_title')}</div>
              <p style={{ margin: 0, color: 'var(--text-secondary)', fontSize: 'var(--font-size-sm)', lineHeight: 1.5 }}>
                {t('rooms.guest_manual_desc')}
              </p>
            </div>
          )}
        </div>
      )}

      <div style={{ display: 'flex', gap: 'var(--space-md)', flexWrap: 'wrap' }}>
        <Button variant="secondary" onClick={() => { refresh(); }}>
          <RefreshCw size={16} style={{ marginRight: 6 }} />
          {t('rooms.refresh')}
        </Button>
        <Button variant="ghost" onClick={() => { leave(); }} loading={busy} disabled={busy}>
          <LogOut size={16} style={{ marginRight: 6 }} />
          {t('rooms.leave')}
        </Button>
      </div>

      {!worldReady && !status.host && (
        <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-sm)', color: 'var(--text-tertiary)', fontSize: 'var(--font-size-sm)' }}>
          <WifiOff size={14} />
          {t('rooms.guest_leave_hint')}
        </div>
      )}
    </div>
  );
}

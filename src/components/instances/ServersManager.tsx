import { useState, useEffect, useCallback, useRef, useMemo } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Server, Plus, Pencil, Trash2, RefreshCw, Play, X } from 'lucide-react';
import { Button } from '../ui/Button';
import { Modal } from '../ui/Modal';
import { Input } from '../ui/Input';
import { Tooltip } from '../ui/Tooltip';
import { addToast } from '../ui/Toast';
import { t } from '../../lib/i18n';
import { useThemeStore } from '../../stores/themeStore';
import { useInstanceStore } from '../../stores/instanceStore';
import { useServerStatuses } from '../../hooks/useServerStatuses';
import { parseServerAddress } from '../../lib/serverAddress';
import { statusLabel, statusPing, pingBarsLevel, type ServerStatus } from '../../lib/serverStatus';

interface ServerEntry {
  name: string;
  ip: string;
  icon: string | null;
  accept_textures: boolean | null;
  hidden: boolean;
}

interface Props {
  instanceName: string;
}

/** Minecraft-style signal staircase: five bars growing from left to right.
 *  While a check runs the steps pulse in a wave with the theme accent colour;
 *  when online the first `level` bars are lit green. */
function PingBars({ level, checking }: { level?: number; checking: boolean }) {
  return (
    <div className={`ping-bars${checking ? ' ping-bars--checking' : ''}`} aria-hidden="true">
      {[0, 1, 2, 3, 4].map((i) => (
        <span
          key={i}
          className={`ping-bars__bar${!checking && level !== undefined && i < level ? ' ping-bars__bar--on' : ''}`}
        />
      ))}
    </div>
  );
}

/** Status cell shown on the right of every server row, mirroring the vanilla
 *  server list: player counts while online, a green staircase scaled by ping,
 *  a red cross when the check failed. Details live in the hover tooltip —
 *  ping for a live server, the honest failure reason otherwise. */
function ServerStatusCell({ status }: { status: ServerStatus | undefined }) {
  const checking = !status || status.checking;

  // First check, nothing known yet: only the animated staircase.
  if (checking && status?.state === undefined) {
    return (
      <div className="server-status">
        <Tooltip content={t('servers.status_checking')} side="left">
          <PingBars checking />
        </Tooltip>
      </div>
    );
  }

  // Re-check with the previous result known: dimmed numbers stay, the
  // staircase animates until the fresh verdict arrives.
  if (checking) {
    return (
      <div className="server-status">
        {status!.state === 'online' && <OnlineNumbers status={status!} stale />}
        <Tooltip content={t('servers.status_checking')} side="left">
          <PingBars checking />
        </Tooltip>
      </div>
    );
  }

  if (status.state === 'online') {
    const tip = statusPing(status, t);
    const bars = <PingBars checking={false} level={pingBarsLevel(typeof status.pingMs === 'number' ? status.pingMs : 0)} />;
    return (
      <div className="server-status">
        <OnlineNumbers status={status} />
        {tip ? <Tooltip content={tip} side="left">{bars}</Tooltip> : bars}
      </div>
    );
  }

  // timeout / unreachable / error — the cross, with the reason on hover.
  return (
    <div className="server-status">
      <Tooltip content={statusLabel(status, t)} side="left">
        <span className="server-status__fail">
          <X size={15} strokeWidth={3.4} />
        </span>
      </Tooltip>
    </div>
  );
}

/** `players/max` with no word labels, exactly like the vanilla list. */
function OnlineNumbers({ status, stale }: { status: ServerStatus; stale?: boolean }) {
  const online = status.playersOnline;
  const max = status.playersMax;
  if (typeof online !== 'number') return null;
  const text = typeof max === 'number' ? `${online}/${max}` : String(online);
  return (
    <span className={`server-status__numbers${stale ? ' server-status__numbers--stale' : ''}`}>
      {text}
    </span>
  );
}

export function ServersManager({ instanceName }: Props) {
  const liquidGlass = useThemeStore((s) => s.liquidGlass);
  const launchGame = useInstanceStore((s) => s.launchGame);
  const isLaunching = useInstanceStore((s) => s.isLaunching);
  const [servers, setServers] = useState<ServerEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [formOpen, setFormOpen] = useState(false);
  const [editIndex, setEditIndex] = useState<number | null>(null);
  const [nameValue, setNameValue] = useState('');
  const [addrValue, setAddrValue] = useState('');
  const [saving, setSaving] = useState(false);
  const [deleteTarget, setDeleteTarget] = useState<{ index: number; name: string } | null>(null);
  const [launchingAddress, setLaunchingAddress] = useState<string | null>(null);
  // Switching instances quickly must not let a slower reply for the previous
  // instance overwrite the list of the new one.
  const requestSeq = useRef(0);

  const loadServers = useCallback(async () => {
    const seq = ++requestSeq.current;
    setLoading(true);
    try {
      const s = await invoke<ServerEntry[]>('cmd_list_servers', { instanceName });
      if (seq !== requestSeq.current) return;
      setServers(s);
    } catch (e: any) {
      if (seq === requestSeq.current) {
        addToast(t('servers.load_error', { error: e.toString() }), 'error');
      }
    } finally {
      if (seq === requestSeq.current) setLoading(false);
    }
  }, [instanceName]);

  useEffect(() => { loadServers(); }, [loadServers]);

  const addresses = useMemo(() => servers.map((s) => s.ip.trim()), [servers]);
  const { statuses, refresh } = useServerStatuses(instanceName, addresses);
  const anyChecking = addresses.some((a) => statuses[a]?.checking);

  const handleRefresh = () => {
    loadServers();
    refresh();
  };

  const openAdd = () => {
    setEditIndex(null);
    setNameValue('');
    setAddrValue('');
    setFormOpen(true);
  };

  const openEdit = (index: number, server: ServerEntry) => {
    setEditIndex(index);
    setNameValue(server.name);
    setAddrValue(server.ip);
    setFormOpen(true);
  };

  const handleSave = async () => {
    const name = nameValue.trim();
    const address = addrValue.trim();
    if (!name || !address) return;
    setSaving(true);
    try {
      const updated = editIndex === null
        ? await invoke<ServerEntry[]>('cmd_add_server', { instanceName, name, address })
        : await invoke<ServerEntry[]>('cmd_update_server', { instanceName, index: editIndex, name, address });
      setServers(updated);
      addToast(t(editIndex === null ? 'servers.added_toast' : 'servers.updated_toast'), 'success');
      setFormOpen(false);
    } catch (e: any) {
      addToast(t('servers.action_error', { error: e.toString() }), 'error');
    }
    setSaving(false);
  };

  const handleDelete = async () => {
    if (!deleteTarget) return;
    try {
      const updated = await invoke<ServerEntry[]>('cmd_delete_server', { instanceName, index: deleteTarget.index });
      setServers(updated);
      addToast(t('servers.deleted_toast'), 'success');
      setDeleteTarget(null);
    } catch (e: any) {
      addToast(t('servers.action_error', { error: e.toString() }), 'error');
    }
  };

  // One-click launch: the instance starts and auto-connects to the saved
  // server (legacy `--server` on old versions, `--quickPlayMultiplayer` on
  // 1.20+ — the backend picks the mechanism). The ping result never blocks
  // the button: a server without a status answer can still accept the game.
  const handlePlay = async (server: ServerEntry) => {
    const parsed = parseServerAddress(server.ip);
    if (!parsed) {
      addToast(t('servers.invalid_address'), 'error');
      return;
    }
    setLaunchingAddress(server.ip);
    try {
      const accepted = await launchGame(instanceName, parsed.host, parsed.port);
      if (accepted) {
        addToast(t('servers.launch_started', { name: server.name }), 'success');
      } else {
        // A loader-install request is already surfaced by its own modal.
        const error = useInstanceStore.getState().error;
        if (error) addToast(t('servers.launch_error', { error }), 'error');
      }
    } finally {
      setLaunchingAddress(null);
    }
  };

  const playBusy = isLaunching || launchingAddress !== null;

  const renderPlayButton = (server: ServerEntry) => {
    const isThis = launchingAddress === server.ip;
    return (
      <Button
        size="sm"
        onClick={() => handlePlay(server)}
        loading={isThis}
        disabled={playBusy && !isThis}
      >
        <Play size={14} /> {t('servers.play')}
      </Button>
    );
  };

  return (
    /* The card layout belongs to the Liquid Glass material only (owner:
       «это всё конкретно для темы liquid glass. так что верни старый вариант
       со списком серверов для обычных тем»). Normal themes keep the original
       flat list inside the section tray. */
    <div className={liquidGlass ? undefined : 'content-manager__pane'} style={{ flex: 1, display: 'flex', flexDirection: 'column', overflow: 'hidden' }}>
      <div style={{ padding: 'var(--space-md) var(--space-2xl)', display: 'flex', alignItems: 'center', gap: 'var(--space-sm)', borderBottom: '1px solid var(--surface-border)', flexShrink: 0 }}>
        <h2 style={{ fontSize: 'var(--font-size-lg)', fontWeight: 600, margin: 0, flex: 1, minWidth: 0, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
          {t('servers.title')}
          <span style={{ fontWeight: 400, color: 'var(--text-tertiary)', fontSize: 'var(--font-size-sm)', marginLeft: 8 }}>
            {t('servers.count', { n: servers.length.toString() })}
          </span>
        </h2>
        <Button size="sm" variant="ghost" onClick={handleRefresh} disabled={anyChecking} style={{ flexShrink: 0 }}>
          <RefreshCw size={14} className={anyChecking ? 'icon-spin' : undefined} /> {t('common.refresh')}
        </Button>
        <Button size="sm" onClick={openAdd} style={{ flexShrink: 0 }}>
          <Plus size={14} /> {t('servers.add')}
        </Button>
      </div>

      <div style={{ flex: 1, overflowY: 'auto', ...(liquidGlass ? { padding: 'var(--space-lg) var(--space-2xl)' } : {}) }}>
        {loading ? (
          <div style={{ padding: 40, textAlign: 'center', color: 'var(--text-tertiary)' }}>{t('common.loading')}</div>
        ) : servers.length === 0 ? (
          <div style={{ padding: 40, textAlign: 'center', color: 'var(--text-secondary)' }}>
            <Server size={32} style={{ opacity: 0.5, marginBottom: 'var(--space-sm)' }} />
            <div>{t('servers.empty')}</div>
            <div style={{ fontSize: 'var(--font-size-sm)', marginTop: 4 }}>{t('servers.hint')}</div>
          </div>
        ) : liquidGlass ? (
          /* Liquid Glass: a material card per server, actions carried by the
             same icon-only ghost buttons the builds header uses (owner:
             «кнопки с иконкой, подсказкой, надпись убери... как во вкладке со
             сборками... с расстоянием и при наведении светлая область»). */
          <div className="glass-row-list" style={{ display: 'flex', flexDirection: 'column', gap: 'var(--space-sm)' }}>
            {servers.map((server, index) => (
              <div
                key={`${server.name}-${index}`}
                className="glass-card"
                style={{ padding: 'var(--space-md)', display: 'flex', alignItems: 'center', gap: 'var(--space-md)' }}
              >
                {server.icon ? (
                  <img
                    src={`data:image/png;base64,${server.icon}`}
                    alt=""
                    style={{ width: 36, height: 36, borderRadius: 'var(--radius-sm)', objectFit: 'cover', flexShrink: 0, imageRendering: 'pixelated' }}
                  />
                ) : (
                  <div style={{ width: 36, height: 36, borderRadius: 'var(--radius-sm)', background: 'var(--surface-glass)', display: 'flex', alignItems: 'center', justifyContent: 'center', flexShrink: 0 }}>
                    <Server size={18} style={{ color: 'var(--text-tertiary)' }} />
                  </div>
                )}
                <div style={{ flex: 1, minWidth: 0 }}>
                  <div style={{ fontWeight: 600, fontSize: 'var(--font-size-sm)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{server.name}</div>
                  <div style={{ fontSize: 'var(--font-size-sm)', color: 'var(--text-secondary)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{server.ip}</div>
                </div>
                <ServerStatusCell status={statuses[server.ip.trim()]} />
                <div style={{ display: 'flex', gap: 'var(--space-sm)', flexShrink: 0, alignItems: 'center' }}>
                  {renderPlayButton(server)}
                  <Tooltip content={t('servers.edit')}>
                    <Button size="sm" variant="ghost" onClick={() => openEdit(index, server)}>
                      <Pencil size={14} />
                    </Button>
                  </Tooltip>
                  <Tooltip content={t('servers.delete')}>
                    <Button size="sm" variant="ghost" onClick={() => setDeleteTarget({ index, name: server.name })} style={{ color: 'var(--color-danger)' }}>
                      <Trash2 size={14} />
                    </Button>
                  </Tooltip>
                </div>
              </div>
            ))}
          </div>
        ) : (
          servers.map((server, index) => (
            <div key={`${server.name}-${index}`} style={{
              display: 'grid', gridTemplateColumns: '1fr auto auto',
              padding: '8px var(--space-2xl)', alignItems: 'center',
              borderBottom: '1px solid var(--surface-border)', gap: 'var(--space-sm)',
            }}>
              <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-sm)', minWidth: 0 }}>
                {server.icon ? (
                  <img
                    src={`data:image/png;base64,${server.icon}`}
                    alt=""
                    style={{ width: 32, height: 32, borderRadius: 'var(--radius-sm)', objectFit: 'cover', flexShrink: 0, imageRendering: 'pixelated' }}
                  />
                ) : (
                  <div style={{ width: 32, height: 32, borderRadius: 'var(--radius-sm)', background: 'var(--surface-glass)', display: 'flex', alignItems: 'center', justifyContent: 'center', flexShrink: 0 }}>
                    <Server size={16} style={{ color: 'var(--text-tertiary)' }} />
                  </div>
                )}
                <div style={{ minWidth: 0 }}>
                  <div style={{ fontWeight: 500, fontSize: 'var(--font-size-sm)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{server.name}</div>
                  <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{server.ip}</div>
                </div>
              </div>
              <ServerStatusCell status={statuses[server.ip.trim()]} />
              <div style={{ display: 'flex', gap: 2, justifyContent: 'flex-end', alignItems: 'center' }}>
                {renderPlayButton(server)}
                <Tooltip content={t('servers.edit')}>
                  <button onClick={() => openEdit(index, server)} style={{ background: 'none', border: 'none', padding: 4, cursor: 'pointer', color: 'var(--text-tertiary)', borderRadius: 'var(--radius-sm)' }}><Pencil size={14} /></button>
                </Tooltip>
                <Tooltip content={t('servers.delete')}>
                  <button onClick={() => setDeleteTarget({ index, name: server.name })} style={{ background: 'none', border: 'none', padding: 4, cursor: 'pointer', color: 'var(--text-tertiary)', borderRadius: 'var(--radius-sm)' }}><Trash2 size={14} /></button>
                </Tooltip>
              </div>
            </div>
          ))
        )}
      </div>

      {/* Add / Edit modal */}
      <Modal open={formOpen} onClose={() => setFormOpen(false)} title={t(editIndex === null ? 'servers.add_title' : 'servers.edit_title')}>
        <Input
          label={t('servers.name_label')}
          id="server-name"
          value={nameValue}
          onChange={(e) => setNameValue(e.target.value)}
          placeholder={t('servers.name_placeholder')}
          maxLength={128}
          autoFocus
        />
        <div style={{ marginTop: 'var(--space-md)' }}>
          <Input
            label={t('servers.address_label')}
            id="server-address"
            value={addrValue}
            onChange={(e) => setAddrValue(e.target.value)}
            placeholder={t('servers.address_placeholder')}
            maxLength={255}
            onKeyDown={(e) => { if (e.key === 'Enter') handleSave(); }}
          />
        </div>
        <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 'var(--space-sm)', marginTop: 'var(--space-lg)' }}>
          <Button variant="ghost" onClick={() => setFormOpen(false)}>{t('common.cancel')}</Button>
          <Button onClick={handleSave} disabled={saving || !nameValue.trim() || !addrValue.trim()}>{t('common.save')}</Button>
        </div>
      </Modal>

      {/* Delete confirmation */}
      <Modal open={!!deleteTarget} onClose={() => setDeleteTarget(null)} title={t('servers.delete_title')}>
        <p style={{ margin: 0, color: 'var(--text-secondary)' }}>
          {t('servers.delete_confirm', { name: deleteTarget?.name ?? '' })}
        </p>
        <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 'var(--space-sm)', marginTop: 'var(--space-lg)' }}>
          <Button variant="ghost" onClick={() => setDeleteTarget(null)}>{t('common.cancel')}</Button>
          <Button variant="danger" onClick={handleDelete}>{t('servers.delete')}</Button>
        </div>
      </Modal>
    </div>
  );
}

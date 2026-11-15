import { useState, useEffect, useCallback, useRef, useMemo } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Server, Plus, Pencil, Trash2, Play, X, Lock } from 'lucide-react';
import { Button } from '../ui/Button';
import { RefreshIcon } from '../ui/RefreshIcon';
import { Modal } from '../ui/Modal';
import { Input } from '../ui/Input';
import { Tooltip } from '../ui/Tooltip';
import { addToast } from '../ui/Toast';
import { t } from '../../lib/i18n';
import { useThemeStore } from '../../stores/themeStore';
import { useInstanceStore } from '../../stores/instanceStore';
import { useFocusStore } from '../../stores/focusStore';
import { useEventStore } from '../../hooks/useGameEvents';
import { useServerStatuses } from '../../hooks/useServerStatuses';
import { parseServerAddress } from '../../lib/serverAddress';
import { statusLabel, statusPing, pingBarsLevel, type ServerStatus } from '../../lib/serverStatus';
import { isServerConflict, serverErrorMessage } from '../../lib/serverErrors';
import {
  isServerEditingLocked,
  responseIsCurrent,
  serversViewState,
  shouldReloadAfterRunningChange,
  shouldReloadOnFocus,
} from '../../lib/serversListState';

interface ServerEntry {
  name: string;
  ip: string;
  icon: string | null;
  accept_textures: boolean | null;
  hidden: boolean;
  fingerprint: string;
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
 *  a red cross when the check failed. Details live in the hover tooltip вЂ”
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

  // timeout / unreachable / error вЂ” the cross, with the reason on hover.
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
  const [hasLoaded, setHasLoaded] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [formOpen, setFormOpen] = useState(false);
  const [editIndex, setEditIndex] = useState<number | null>(null);
  const [editOriginal, setEditOriginal] = useState<{ name: string; ip: string; fingerprint: string } | null>(null);
  const [nameValue, setNameValue] = useState('');
  const [addrValue, setAddrValue] = useState('');
  const [saving, setSaving] = useState(false);
  const [deleteTarget, setDeleteTarget] = useState<{ index: number; name: string; ip: string; fingerprint: string } | null>(null);
  const [launchingAddress, setLaunchingAddress] = useState<string | null>(null);
  // Switching instances quickly must not let a slower reply for the previous
  // instance overwrite the list of the new one.
  const requestSeq = useRef(0);
  const loadInFlight = useRef<{ instance: string; promise: Promise<void> } | null>(null);
  const lastLifecycleLoad = useRef(0);
  const prevInstance = useRef(instanceName);
  const activeInstance = useRef(instanceName);
  activeInstance.current = instanceName;
  const formOpenRef = useRef(formOpen);
  formOpenRef.current = formOpen;
  const deleteTargetRef = useRef(deleteTarget);
  deleteTargetRef.current = deleteTarget;

  const instanceRunning = useEventStore((s) =>
    isServerEditingLocked(s.runningGameIds, instanceName),
  );
  const isWindowFocused = useFocusStore((s) => s.isWindowFocused);
  const isMinimized = useFocusStore((s) => s.isMinimized);

  const loadServers = useCallback(async () => {
    const active = loadInFlight.current;
    if (active && active.instance === instanceName) return active.promise;

    const seq = ++requestSeq.current;
    setLoading(true);
    setLoadError(null);
    const entry: { instance: string; promise: Promise<void> } = {
      instance: instanceName,
      promise: Promise.resolve(),
    };
    const promise = (async () => {
      try {
        const s = await invoke<ServerEntry[]>('cmd_list_servers', { instanceName });
        if (
          !responseIsCurrent({
            responseSeq: seq,
            currentSeq: requestSeq.current,
            responseInstance: instanceName,
            currentInstance: activeInstance.current,
          })
        ) return;
        setServers(s);
        setHasLoaded(true);
      } catch (e: any) {
        if (
          !responseIsCurrent({
            responseSeq: seq,
            currentSeq: requestSeq.current,
            responseInstance: instanceName,
            currentInstance: activeInstance.current,
          })
        ) return;
        const message = String(e);
        setLoadError(message);
        addToast(t('servers.load_error', { error: message }), 'error');
      } finally {
        if (
          responseIsCurrent({
            responseSeq: seq,
            currentSeq: requestSeq.current,
            responseInstance: instanceName,
            currentInstance: activeInstance.current,
          })
        ) setLoading(false);
        if (loadInFlight.current === entry) loadInFlight.current = null;
      }
    })();
    entry.promise = promise;
    loadInFlight.current = entry;
    return promise;
  }, [instanceName]);

  // Reopening the tab or switching the instance always re-reads the actual
  // file; the previous instance's pending response is invalidated by seq.
  useEffect(() => {
    if (prevInstance.current !== instanceName) {
      prevInstance.current = instanceName;
      activeInstance.current = instanceName;
      requestSeq.current += 1;
      setServers([]);
      setHasLoaded(false);
      setLoadError(null);
    }
    void loadServers();
  }, [instanceName, loadServers]);

  const addresses = useMemo(() => servers.map((s) => s.ip.trim()), [servers]);
  const { statuses, refresh } = useServerStatuses(instanceName, addresses);
  const anyChecking = addresses.some((a) => statuses[a]?.checking);

  // Lifecycle refreshes are coalesced: focus, restore and visibility can all
  // fire together after the user returns from Minecraft.
  const loadOnLifecycle = useCallback(() => {
    // A background refresh must not change row indices under an open edit or
    // delete dialog. The user can refresh explicitly after closing it.
    if (formOpenRef.current || deleteTargetRef.current) return;
    const now = Date.now();
    if (now - lastLifecycleLoad.current < 500) return;
    lastLifecycleLoad.current = now;
    void loadServers();
  }, [loadServers]);

  const wasFocused = useRef(isWindowFocused);
  useEffect(() => {
    if (shouldReloadOnFocus(wasFocused.current, isWindowFocused)) loadOnLifecycle();
    wasFocused.current = isWindowFocused;
  }, [isWindowFocused, loadOnLifecycle]);

  const wasMinimized = useRef(isMinimized);
  useEffect(() => {
    if (wasMinimized.current && !isMinimized) loadOnLifecycle();
    wasMinimized.current = isMinimized;
  }, [isMinimized, loadOnLifecycle]);

  useEffect(() => {
    const onVisibility = () => {
      if (!document.hidden) loadOnLifecycle();
    };
    document.addEventListener('visibilitychange', onVisibility);
    return () => document.removeEventListener('visibilitychange', onVisibility);
  }, [loadOnLifecycle]);

  const wasRunning = useRef(instanceRunning);
  useEffect(() => {
    if (shouldReloadAfterRunningChange(wasRunning.current, instanceRunning)) {
      // The game may have rewritten the list on exit. Close a dialog that was
      // opened before the exit so its index cannot target a different row.
      setFormOpen(false);
      setDeleteTarget(null);
      void loadServers();
      refresh();
    }
    wasRunning.current = instanceRunning;
  }, [instanceRunning, loadServers, refresh]);

  const handleRefresh = () => {
    void loadServers();
    refresh();
  };

  const openAdd = () => {
    if (instanceRunning) {
      addToast(t('servers.error_running'), 'warning');
      return;
    }
    setEditIndex(null);
    setEditOriginal(null);
    setNameValue('');
    setAddrValue('');
    setFormOpen(true);
  };

  const openEdit = (index: number, server: ServerEntry) => {
    if (instanceRunning) {
      addToast(t('servers.error_running'), 'warning');
      return;
    }
    setEditIndex(index);
    setEditOriginal({ name: server.name, ip: server.ip, fingerprint: server.fingerprint });
    setNameValue(server.name);
    setAddrValue(server.ip);
    setFormOpen(true);
  };

  const handleSave = async () => {
    if (instanceRunning) {
      addToast(t('servers.error_running'), 'warning');
      return;
    }
    const name = nameValue.trim();
    const address = addrValue.trim();
    if (!name || !address) return;
    setSaving(true);
    try {
      const updated = editIndex === null
        ? await invoke<ServerEntry[]>('cmd_add_server', { instanceName, name, address })
        : await invoke<ServerEntry[]>('cmd_update_server', {
            instanceName,
            index: editIndex,
            name,
            address,
            expectedName: editOriginal?.name ?? null,
            expectedIp: editOriginal?.ip ?? null,
            expectedFingerprint: editOriginal?.fingerprint ?? null,
          });
      setServers(updated);
      setHasLoaded(true);
      // Any list read that started before this successful mutation must not
      // overwrite its result when it resolves later.
      requestSeq.current += 1;
      loadInFlight.current = null;
      setLoading(false);
      addToast(t(editIndex === null ? 'servers.added_toast' : 'servers.updated_toast'), 'success');
      setEditOriginal(null);
      setFormOpen(false);
    } catch (e: any) {
      addToast(serverErrorMessage(e, t), 'error');
      if (isServerConflict(e)) {
        // Keep the user's typed values and the original fingerprint. The
        // backend has already refused the stale edit; reload the list behind
        // the open form so the user can review/cancel without losing input.
        void loadServers();
      }
    }
    setSaving(false);
  };

  const handleDelete = async () => {
    if (!deleteTarget) return;
    if (instanceRunning) {
      addToast(t('servers.error_running'), 'warning');
      return;
    }
    try {
      const updated = await invoke<ServerEntry[]>('cmd_delete_server', {
        instanceName,
        index: deleteTarget.index,
        expectedName: deleteTarget.name,
        expectedIp: deleteTarget.ip,
        expectedFingerprint: deleteTarget.fingerprint,
      });
      setServers(updated);
      setHasLoaded(true);
      requestSeq.current += 1;
      loadInFlight.current = null;
      setLoading(false);
      addToast(t('servers.deleted_toast'), 'success');
      setDeleteTarget(null);
    } catch (e: any) {
      addToast(serverErrorMessage(e, t), 'error');
      if (isServerConflict(e)) {
        setDeleteTarget(null);
        void loadServers();
      }
    }
  };

  // One-click launch: the instance starts and auto-connects to the saved
  // server (legacy `--server` on old versions, `--quickPlayMultiplayer` on
  // 1.20+ вЂ” the backend picks the mechanism). The ping result never blocks
  // the button: a server without a status answer can still accept the game.
  const handlePlay = async (server: ServerEntry) => {
    const parsed = parseServerAddress(server.ip);
    if (!parsed) {
      addToast(t('servers.invalid_address'), 'error');
      return;
    }
    setLaunchingAddress(server.ip);
    try {
      const accepted = await launchGame(instanceName, parsed.host, parsed.port, {
        silentError: true,
      });
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
  const viewState = serversViewState({
    loading,
    hasLoaded,
    error: loadError,
    count: servers.length,
  });

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
       В«СЌС‚Рѕ РІСЃС‘ РєРѕРЅРєСЂРµС‚РЅРѕ РґР»СЏ С‚РµРјС‹ liquid glass. С‚Р°Рє С‡С‚Рѕ РІРµСЂРЅРё СЃС‚Р°СЂС‹Р№ РІР°СЂРёР°РЅС‚
       СЃРѕ СЃРїРёСЃРєРѕРј СЃРµСЂРІРµСЂРѕРІ РґР»СЏ РѕР±С‹С‡РЅС‹С… С‚РµРјВ»). Normal themes keep the original
       flat list inside the section tray. */
    <div className={`content-manager__pane${liquidGlass ? ' content-manager__pane--glassless' : ''}`} style={{ flex: 1, display: 'flex', flexDirection: 'column', overflow: 'hidden' }}>
      <div style={{ padding: 'var(--space-md) var(--space-2xl)', display: 'flex', alignItems: 'center', gap: 'var(--space-sm)', borderBottom: '1px solid var(--surface-border)', flexShrink: 0 }}>
        <h2 style={{ fontSize: 'var(--font-size-lg)', fontWeight: 600, margin: 0, flex: 1, minWidth: 0, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
          {t('servers.title')}
          <span style={{ fontWeight: 400, color: 'var(--text-tertiary)', fontSize: 'var(--font-size-sm)', marginLeft: 8 }}>
            {t('servers.count', { n: servers.length.toString() })}
          </span>
        </h2>
        <Button size="sm" variant="ghost" onClick={handleRefresh} disabled={anyChecking || loading} style={{ flexShrink: 0 }}>
          <RefreshIcon spinning={anyChecking} /> {t('common.refresh')}
        </Button>
        <Button
          size="sm"
          onClick={openAdd}
          disabled={instanceRunning}
          title={instanceRunning ? t('servers.locked_running') : undefined}
          style={{ flexShrink: 0 }}
        >
          <Plus size={14} /> {t('servers.add')}
        </Button>
      </div>

      {instanceRunning && (
        <div
          role="status"
          style={{
            padding: 'var(--space-sm) var(--space-2xl)',
            borderBottom: '1px solid var(--surface-border)',
            display: 'flex',
            alignItems: 'flex-start',
            gap: 'var(--space-sm)',
            fontSize: 'var(--font-size-sm)',
            color: 'var(--text-secondary)',
            flexShrink: 0,
          }}
        >
          <Lock size={15} style={{ flexShrink: 0, marginTop: 1, color: 'var(--color-warning)' }} />
          <span>{t('servers.locked_running')}</span>
        </div>
      )}

      {hasLoaded && loadError && (
        <div
          role="status"
          style={{
            padding: 'var(--space-sm) var(--space-2xl)',
            borderBottom: '1px solid var(--surface-border)',
            fontSize: 'var(--font-size-sm)',
            color: 'var(--text-secondary)',
            flexShrink: 0,
          }}
        >
          {t('servers.load_error', { error: loadError })}
        </div>
      )}

      <div style={{ flex: 1, overflowY: 'auto', ...(liquidGlass ? { padding: 'var(--space-xs) var(--space-lg) var(--space-lg)' } : {}) }}>
        {viewState === 'loading' ? (
          <div style={{ padding: 40, textAlign: 'center', color: 'var(--text-tertiary)' }}>{t('common.loading')}</div>
        ) : viewState === 'error' ? (
          <div style={{ padding: 40, textAlign: 'center', color: 'var(--text-secondary)' }}>
            <div>{t('servers.load_error', { error: loadError ?? '' })}</div>
            <div style={{ marginTop: 'var(--space-md)' }}>
              <Button size="sm" variant="secondary" onClick={handleRefresh}>{t('common.retry')}</Button>
            </div>
          </div>
        ) : viewState === 'empty' ? (
          <div style={{ padding: 40, textAlign: 'center', color: 'var(--text-secondary)' }}>
            <Server size={32} style={{ opacity: 0.5, marginBottom: 'var(--space-sm)' }} />
            <div>{t('servers.empty')}</div>
            <div style={{ fontSize: 'var(--font-size-sm)', marginTop: 4 }}>{t('servers.hint')}</div>
          </div>
        ) : liquidGlass ? (
          /* Liquid Glass: a material card per server, actions carried by the
             same icon-only ghost buttons the builds header uses (owner:
             В«РєРЅРѕРїРєРё СЃ РёРєРѕРЅРєРѕР№, РїРѕРґСЃРєР°Р·РєРѕР№, РЅР°РґРїРёСЃСЊ СѓР±РµСЂРё... РєР°Рє РІРѕ РІРєР»Р°РґРєРµ СЃРѕ
             СЃР±РѕСЂРєР°РјРё... СЃ СЂР°СЃСЃС‚РѕСЏРЅРёРµРј Рё РїСЂРё РЅР°РІРµРґРµРЅРёРё СЃРІРµС‚Р»Р°СЏ РѕР±Р»Р°СЃС‚СЊВ»). */
          <div className="glass-row-list" style={{ display: 'flex', flexDirection: 'column', gap: 'var(--row-card-gap)' }}>
            {servers.map((server, index) => (
              <div
                key={`${server.name}-${index}`}
                className="glass-card"
                style={{ padding: 'var(--row-padding-y) var(--space-lg)', display: 'flex', alignItems: 'center', gap: 'var(--space-sm)' }}
              >
                {server.icon ? (
                  <img
                    src={`data:image/png;base64,${server.icon}`}
                    alt=""
                    style={{ width: 'var(--row-icon-size)', height: 'var(--row-icon-size)', borderRadius: 'var(--radius-sm)', objectFit: 'cover', flexShrink: 0, imageRendering: 'pixelated' }}
                  />
                ) : (
                  <div style={{ width: 'var(--row-icon-size)', height: 'var(--row-icon-size)', borderRadius: 'var(--radius-sm)', background: 'var(--surface-glass)', display: 'flex', alignItems: 'center', justifyContent: 'center', flexShrink: 0 }}>
                    <Server size={16} style={{ color: 'var(--text-tertiary)' }} />
                  </div>
                )}
                <div style={{ flex: 1, minWidth: 0 }}>
                  <div style={{ fontWeight: 500, fontSize: 'var(--font-size-sm)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{server.name}</div>
                  <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{server.ip}</div>
                </div>
                <ServerStatusCell status={statuses[server.ip.trim()]} />
                <div style={{ display: 'flex', gap: 2, flexShrink: 0, alignItems: 'center' }}>
                  {renderPlayButton(server)}
                  <Tooltip content={t('servers.edit')}>
                    <Button className="btn--icon-only" size="sm" variant="ghost" aria-label={t('servers.edit')} onClick={() => openEdit(index, server)} disabled={instanceRunning}>
                      <Pencil size={14} />
                    </Button>
                  </Tooltip>
                  <Tooltip content={t('servers.delete')}>
                    <Button className="btn--icon-only" size="sm" variant="ghost" aria-label={t('servers.delete')} onClick={() => setDeleteTarget({ index, name: server.name, ip: server.ip, fingerprint: server.fingerprint })} disabled={instanceRunning} style={{ color: 'var(--color-danger)' }}>
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
              padding: 'var(--row-padding-y) var(--space-2xl)', alignItems: 'center',
              borderBottom: '1px solid var(--surface-border)', gap: 'var(--space-sm)',
            }}>
              <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-sm)', minWidth: 0 }}>
                {server.icon ? (
                  <img
                    src={`data:image/png;base64,${server.icon}`}
                    alt=""
                    style={{ width: 'var(--row-icon-size)', height: 'var(--row-icon-size)', borderRadius: 'var(--radius-sm)', objectFit: 'cover', flexShrink: 0, imageRendering: 'pixelated' }}
                  />
                ) : (
                  <div style={{ width: 'var(--row-icon-size)', height: 'var(--row-icon-size)', borderRadius: 'var(--radius-sm)', background: 'var(--surface-glass)', display: 'flex', alignItems: 'center', justifyContent: 'center', flexShrink: 0 }}>
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
                  <button disabled={instanceRunning} onClick={() => openEdit(index, server)} aria-label={t('servers.edit')} style={{ background: 'none', border: 'none', padding: 4, cursor: instanceRunning ? 'not-allowed' : 'pointer', opacity: instanceRunning ? 0.4 : 1, color: 'var(--text-tertiary)', borderRadius: 'var(--radius-sm)' }}><Pencil size={14} /></button>
                </Tooltip>
                <Tooltip content={t('servers.delete')}>
                  <button
                    type="button"
                    disabled={instanceRunning}
                    onClick={() => setDeleteTarget({ index, name: server.name, ip: server.ip, fingerprint: server.fingerprint })}
                    className="icon-action icon-action--danger"
                    aria-label={t('servers.delete')}
                  >
                    <Trash2 size={14} />
                  </button>
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
          <Button onClick={handleSave} disabled={saving || instanceRunning || !nameValue.trim() || !addrValue.trim()}>{t('common.save')}</Button>
        </div>
      </Modal>

      {/* Delete confirmation */}
      <Modal open={!!deleteTarget} onClose={() => setDeleteTarget(null)} title={t('servers.delete_title')}>
        <p style={{ margin: 0, color: 'var(--text-secondary)' }}>
          {t('servers.delete_confirm', { name: deleteTarget?.name ?? '' })}
        </p>
        <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 'var(--space-sm)', marginTop: 'var(--space-lg)' }}>
          <Button variant="ghost" onClick={() => setDeleteTarget(null)}>{t('common.cancel')}</Button>
          <Button variant="danger" onClick={handleDelete} disabled={instanceRunning}>{t('servers.delete')}</Button>
        </div>
      </Modal>
    </div>
  );
}

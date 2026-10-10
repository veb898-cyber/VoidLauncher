import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { CheckCircle2, Loader2, Package, RefreshCw, AlertTriangle } from 'lucide-react';
import { Modal } from '../ui/Modal';
import { Button } from '../ui/Button';
import { ProgressBar } from '../ui/ProgressBar';
import { addToast } from '../ui/Toast';
import { useT, type MessageKey } from '../../lib/i18n';

interface PackLock {
  provider: string;
  pack_project_id: string;
  pack_name: string;
  file_id: string;
  file_name: string;
  file_display_name: string;
  installed_at: string;
  mc_version: string;
  loader: string;
}

interface PackUpdateInfo {
  up_to_date: boolean;
  current_file_id: string;
  current_version: string;
  latest_file_id: string | null;
  latest_version: string | null;
  latest_file_name: string | null;
}

interface PackUpdateReport {
  mods_installed: number;
  mods_removed: number;
  overrides_written: number;
  overrides_removed: number;
  new_version: string;
}

interface PackRepairReport {
  checked: number;
  restored: number;
  skipped: number;
  failed: string[];
}

interface ProgressPayload {
  stage: string;
  current: number;
  total: number;
  message: string;
}

const STAGE_LABELS: Record<string, MessageKey> = {
  'updating-pack': 'pack.stage_updating_pack',
  'downloading-pack': 'pack.stage_downloading_pack',
  'downloading-mods': 'pack.stage_downloading_mods',
  'applying': 'pack.stage_applying',
  'repairing': 'pack.stage_repairing',
  'done': 'pack.stage_done',
};

interface Props {
  open: boolean;
  instanceName: string;
  onClose: () => void;
}

/** Update / repair panel for instances installed from a modpack
 *  (driven by the `<instance>/pack-lock.json` lock file — CurseForge and
 *  Modrinth packs are both tracked). */
export function PackModal({ open, instanceName, onClose }: Props) {
  const t = useT();
  const [lock, setLock] = useState<PackLock | null>(null);
  const [update, setUpdate] = useState<PackUpdateInfo | null>(null);
  const [checking, setChecking] = useState(false);
  const [busy, setBusy] = useState(false);
  const busyRef = useRef(false);
  const [progress, setProgress] = useState<ProgressPayload | null>(null);
  const [checkError, setCheckError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    setChecking(true);
    setCheckError(null);
    try {
      const l = await invoke<PackLock | null>('cmd_get_pack_lock', { instanceName });
      setLock(l);
      if (l) {
        try {
          setUpdate(await invoke<PackUpdateInfo>('cmd_check_modpack_update', { instanceName }));
        } catch (e: any) {
          setUpdate(null);
          setCheckError(e.toString());
        }
      } else {
        setUpdate(null);
      }
    } catch (e: any) {
      setCheckError(e.toString());
    } finally {
      setChecking(false);
    }
  }, [instanceName]);

  useEffect(() => {
    if (open) {
      setProgress(null);
      refresh();
    }
  }, [open, refresh]);

  // Progress from cmd_update_modpack / cmd_repair_modpack.
  useEffect(() => {
    if (!open) return;
    let unlisten: (() => void) | null = null;
    let dead = false;
    listen<ProgressPayload>('import-progress', (event) => {
      if (busyRef.current && event.payload) setProgress(event.payload);
    }).then((fn) => {
      if (dead) fn();
      else unlisten = fn;
    }).catch(() => {});
    return () => {
      dead = true;
      if (unlisten) unlisten();
    };
  }, [open]);

  const runBusy = async (fn: () => Promise<void>) => {
    busyRef.current = true;
    setBusy(true);
    setProgress(null);
    try {
      await fn();
    } catch (e: any) {
      addToast(t('pack.error', { error: e.toString() }), 'error');
    } finally {
      busyRef.current = false;
      setBusy(false);
      setProgress(null);
      refresh();
    }
  };

  const handleUpdate = () => runBusy(async () => {
    const rep = await invoke<PackUpdateReport>('cmd_update_modpack', { instanceName });
    addToast(
      t('pack.update_summary', {
        version: rep.new_version,
        installed: rep.mods_installed,
        removed: rep.mods_removed,
        overrides: rep.overrides_written + rep.overrides_removed,
      }),
      'success',
    );
  });

  const handleRepair = () => runBusy(async () => {
    const rep = await invoke<PackRepairReport>('cmd_repair_modpack', { instanceName });
    if (rep.failed.length > 0) {
      addToast(
        t('pack.repair_partial', { restored: rep.restored, checked: rep.checked, files: rep.failed.join(', ') }),
        'error',
      );
    } else {
      addToast(t('pack.repair_success', { restored: rep.restored, checked: rep.checked }), 'success');
    }
  });

  const stageLabel = progress
    ? (STAGE_LABELS[progress.stage] ? t(STAGE_LABELS[progress.stage]) : progress.message)
    : null;
  const showPercent = !!progress && progress.total > 1;

  return (
    <Modal
      open={open}
      onClose={busy ? () => {} : onClose}
      title={t('pack.title')}
      maxWidth={520}
      footer={
        <div style={{ display: 'flex', gap: 'var(--space-md)', justifyContent: 'flex-end', width: '100%' }}>
          <Button variant="ghost" onClick={onClose} disabled={busy}>
            {t('common.close')}
          </Button>
          <Button
            variant="ghost"
            onClick={handleRepair}
            disabled={busy || !lock || checking}
          >
            {t('pack.btn_repair')}
          </Button>
          <Button
            onClick={handleUpdate}
            disabled={busy || !lock || checking || !update || update.up_to_date}
          >
            {t('pack.btn_update')}
          </Button>
        </div>
      }
    >
      {!lock ? (
        <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-md)', padding: 'var(--space-lg) 0', color: 'var(--text-secondary)' }}>
          {checking ? (
            <>
              <Loader2 size={16} className="animate-spin" />
              <span style={{ fontSize: 'var(--font-size-sm)' }}>{t('pack.checking')}</span>
            </>
          ) : (
            <>
              <Package size={16} />
              <span style={{ fontSize: 'var(--font-size-sm)' }}>{t('pack.no_lock')}</span>
            </>
          )}
        </div>
      ) : (
        <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--space-md)' }}>
          {/* Pack header */}
          <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-md)' }}>
            <Package size={20} style={{ color: 'var(--text-secondary)', flexShrink: 0 }} />
            <div style={{ minWidth: 0 }}>
              <div style={{ fontSize: 'var(--font-size-md)', fontWeight: 600, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                {lock.pack_name}
              </div>
              <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)' }}>
                {lock.mc_version}{lock.loader ? ` / ${lock.loader}` : ''}
              </div>
            </div>
          </div>

          {/* Versions */}
          <div style={{ display: 'flex', flexDirection: 'column', gap: 4, fontSize: 'var(--font-size-sm)' }}>
            <div style={{ display: 'flex', justifyContent: 'space-between', gap: 'var(--space-md)' }}>
              <span style={{ color: 'var(--text-secondary)' }}>{t('pack.current_version')}</span>
              <span style={{ fontWeight: 500, textAlign: 'right', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                {lock.file_display_name}
              </span>
            </div>
            <div style={{ display: 'flex', justifyContent: 'space-between', gap: 'var(--space-md)' }}>
              <span style={{ color: 'var(--text-secondary)' }}>{t('pack.latest_version')}</span>
              <span style={{ fontWeight: 500, textAlign: 'right', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                {checking ? '...' : (update?.latest_version ?? '-')}
              </span>
            </div>
          </div>

          {/* Status */}
          {!checking && !checkError && update && (
            <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-sm)', fontSize: 'var(--font-size-sm)' }}>
              {update.up_to_date ? (
                <>
                  <CheckCircle2 size={15} style={{ color: 'var(--success)' }} />
                  <span style={{ color: 'var(--success)' }}>{t('pack.up_to_date')}</span>
                </>
              ) : (
                <>
                  <RefreshCw size={14} style={{ color: 'var(--primary)' }} />
                  <span style={{ color: 'var(--primary)' }}>
                    {t('pack.update_available', { version: update.latest_version ?? '' })}
                  </span>
                </>
              )}
            </div>
          )}
          {checkError && (
            <div style={{ display: 'flex', alignItems: 'flex-start', gap: 'var(--space-sm)', fontSize: 'var(--font-size-xs)', color: 'var(--warning)' }}>
              <AlertTriangle size={14} style={{ flexShrink: 0, marginTop: 1 }} />
              <span>{t('pack.check_error', { error: checkError })}</span>
            </div>
          )}

          {/* Live progress while update/repair runs */}
          {busy && stageLabel && (
            <div style={{ display: 'flex', flexDirection: 'column', gap: 6 }}>
              <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-sm)', fontSize: 'var(--font-size-sm)', color: 'var(--text-secondary)' }}>
                <Loader2 size={14} className="animate-spin" />
                <span>{stageLabel}</span>
                {progress && progress.total > 0 && !showPercent && (
                  <span style={{ color: 'var(--text-tertiary)', fontSize: 'var(--font-size-xs)' }}>
                    {progress.current}/{progress.total}
                  </span>
                )}
              </div>
              {showPercent && progress && (
                <ProgressBar percent={Math.round((progress.current / progress.total) * 100)} />
              )}
            </div>
          )}
        </div>
      )}
    </Modal>
  );
}

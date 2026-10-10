import { useCallback, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Archive, Trash2 } from 'lucide-react';
import { Modal } from '../ui/Modal';
import { Button } from '../ui/Button';
import { Input } from '../ui/Input';
import { Tooltip } from '../ui/Tooltip';
import { addToast } from '../ui/Toast';
import { t } from '../../lib/i18n';
import { formatSize } from '../../lib/format';

interface BackupInfo {
  filename: string;
  size_bytes: number;
  created_at: number;
}

interface Props {
  instanceName: string;
  onClose: () => void;
  /** Called after a successful restore so the caller can reload the list. */
  onRestored: () => void;
}

/**
 * Lists an instance's stored backups and restores one: either replacing the
 * instance in place (its `.backups` history survives the swap) or extracting
 * it as a brand-new instance under a chosen name. Each backup can also be
 * deleted here.
 */
export function InstanceRestoreModal({ instanceName, onClose, onRestored }: Props) {
  const [backups, setBackups] = useState<BackupInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [selected, setSelected] = useState<BackupInfo | null>(null);
  const [asNewOpen, setAsNewOpen] = useState(false);
  const [asNewName, setAsNewName] = useState('');
  const [replaceConfirm, setReplaceConfirm] = useState(false);
  const [deleteTarget, setDeleteTarget] = useState<BackupInfo | null>(null);
  const [busy, setBusy] = useState(false);

  const formatDate = (ts: number) =>
    new Date(ts * 1000).toLocaleDateString(undefined, {
      year: 'numeric',
      month: 'short',
      day: 'numeric',
      hour: '2-digit',
      minute: '2-digit',
    });

  const loadBackups = useCallback(async () => {
    setLoading(true);
    try {
      const list = await invoke<BackupInfo[]>('cmd_list_instance_backups', { name: instanceName });
      setBackups(list);
      setSelected(list[0] ?? null);
    } catch (e: any) {
      addToast(t('instances.restore_error', { error: e.toString() }), 'error');
    }
    setLoading(false);
  }, [instanceName]);

  useEffect(() => { loadBackups(); }, [loadBackups]);

  const doRestore = async (mode: 'replace' | 'as_new', newName?: string) => {
    if (!selected) return;
    setBusy(true);
    try {
      await invoke('cmd_restore_instance', {
        name: instanceName,
        backupFilename: selected.filename,
        mode,
        newName: newName ?? null,
      });
      if (mode === 'replace') {
        addToast(t('instances.restore_toast'), 'success');
      } else {
        addToast(t('instances.restore_as_new_toast', { name: newName ?? '' }), 'success');
      }
      onRestored();
      onClose();
    } catch (e: any) {
      addToast(t('instances.restore_error', { error: e.toString() }), 'error');
    }
    setBusy(false);
  };

  const handleDeleteBackup = async () => {
    if (!deleteTarget) return;
    try {
      await invoke('cmd_delete_instance_backup', {
        name: instanceName,
        backupFilename: deleteTarget.filename,
      });
      addToast(t('instances.backup_delete_toast'), 'success');
      setDeleteTarget(null);
      loadBackups();
    } catch (e: any) {
      addToast(t('instances.restore_error', { error: e.toString() }), 'error');
    }
  };

  return (
    <>
      <Modal open onClose={onClose} title={t('instances.restore_title', { name: instanceName })} maxWidth={520}>
        {loading ? (
          <div style={{ padding: 24, textAlign: 'center', color: 'var(--text-tertiary)' }}>
            {t('common.loading')}
          </div>
        ) : backups.length === 0 ? (
          <div style={{ padding: 24, textAlign: 'center', color: 'var(--text-tertiary)' }}>
            <Archive size={28} style={{ opacity: 0.3, marginBottom: 'var(--space-sm)' }} />
            <div>{t('instances.restore_empty')}</div>
            <div style={{ fontSize: 'var(--font-size-sm)', marginTop: 4 }}>{t('instances.restore_hint')}</div>
          </div>
        ) : (
          <>
            <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)', marginBottom: 'var(--space-sm)' }}>
              {t('instances.backups_count', { n: backups.length.toString() })}
            </div>
            <div style={{ maxHeight: 260, overflowY: 'auto', display: 'flex', flexDirection: 'column', gap: 4 }}>
              {backups.map((b) => (
                <div
                  key={b.filename}
                  onClick={() => setSelected(b)}
                  style={{
                    display: 'flex',
                    alignItems: 'center',
                    gap: 'var(--space-sm)',
                    padding: '8px 10px',
                    borderRadius: 'var(--radius-sm)',
                    cursor: 'pointer',
                    border: selected?.filename === b.filename ? '1px solid var(--accent)' : '1px solid var(--surface-border)',
                    background: selected?.filename === b.filename ? 'var(--accent-dim, rgba(255,255,255,0.04))' : 'transparent',
                    transition: 'border-color 160ms ease, background 160ms ease',
                  }}
                >
                  <div style={{ flex: 1, minWidth: 0 }}>
                    <div style={{ fontSize: 'var(--font-size-sm)', fontWeight: 500 }}>{formatDate(b.created_at)}</div>
                    <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)' }}>{formatSize(b.size_bytes)}</div>
                  </div>
                  <Tooltip content={t('instances.backup_delete')}>
                    <button
                      onClick={(e) => { e.stopPropagation(); setDeleteTarget(b); }}
                      style={{ background: 'none', border: 'none', padding: 4, cursor: 'pointer', color: 'var(--text-tertiary)', borderRadius: 'var(--radius-sm)' }}
                    >
                      <Trash2 size={14} />
                    </button>
                  </Tooltip>
                </div>
              ))}
            </div>
            <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 'var(--space-sm)', marginTop: 'var(--space-lg)' }}>
              <Button variant="ghost" onClick={onClose}>{t('common.cancel')}</Button>
              <Button
                variant="ghost"
                disabled={!selected}
                onClick={() => { setAsNewName(`${instanceName} ${t('worlds.copy_suffix')}`.trim()); setAsNewOpen(true); }}
              >
                {t('instances.restore_as_new')}
              </Button>
              <Button variant="danger" disabled={!selected} onClick={() => setReplaceConfirm(true)}>
                {t('instances.restore_replace')}
              </Button>
            </div>
          </>
        )}
      </Modal>

      {/* Restore as new name */}
      <Modal open={asNewOpen} onClose={() => setAsNewOpen(false)} title={t('instances.restore_as_new')}>
        <Input
          label={t('instances.restore_as_new_label')}
          id="instance-restore-asnew"
          value={asNewName}
          onChange={(e) => setAsNewName(e.target.value)}
          autoFocus
        />
        <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 'var(--space-sm)', marginTop: 'var(--space-lg)' }}>
          <Button variant="ghost" onClick={() => setAsNewOpen(false)}>{t('common.cancel')}</Button>
          <Button disabled={!asNewName.trim() || busy} onClick={() => doRestore('as_new', asNewName.trim())}>
            {t('common.confirm')}
          </Button>
        </div>
      </Modal>

      {/* Replace-instance confirmation */}
      <Modal open={replaceConfirm} onClose={() => setReplaceConfirm(false)} title={t('instances.restore_replace')}>
        <p style={{ margin: 0, color: 'var(--text-secondary)' }}>
          {t('instances.restore_replace_confirm', { name: instanceName })}
        </p>
        <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 'var(--space-sm)', marginTop: 'var(--space-lg)' }}>
          <Button variant="ghost" onClick={() => setReplaceConfirm(false)}>{t('common.cancel')}</Button>
          <Button variant="danger" disabled={busy} onClick={() => doRestore('replace')}>
            {t('instances.restore_replace')}
          </Button>
        </div>
      </Modal>

      {/* Delete backup confirmation */}
      <Modal open={!!deleteTarget} onClose={() => setDeleteTarget(null)} title={t('instances.backup_delete')}>
        <p style={{ margin: 0, color: 'var(--text-secondary)' }}>
          {t('instances.backup_delete_confirm', { filename: deleteTarget?.filename ?? '' })}
        </p>
        <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 'var(--space-sm)', marginTop: 'var(--space-lg)' }}>
          <Button variant="ghost" onClick={() => setDeleteTarget(null)}>{t('common.cancel')}</Button>
          <Button variant="danger" onClick={handleDeleteBackup}>{t('common.confirm')}</Button>
        </div>
      </Modal>
    </>
  );
}

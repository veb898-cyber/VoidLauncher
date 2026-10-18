import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Globe, FolderOpen, Pencil, Copy, Trash2, RefreshCw, Archive, RotateCcw } from 'lucide-react';
import { Button } from '../ui/Button';
import { CopyButton } from '../ui/CopyButton';
import { Tooltip } from '../ui/Tooltip';
import { Modal } from '../ui/Modal';
import { Input } from '../ui/Input';
import { addToast } from '../ui/Toast';
import { t } from '../../lib/i18n';
import { useThemeStore } from '../../stores/themeStore';
import { formatSize as fmtSize } from '../../lib/format';

interface SaveEntry {
  name: string;
  last_modified: number | null;
  size_bytes: number;
  game_mode: string | null;
  seed: number | null;
  icon_data: string | null;
}

interface BackupInfo {
  filename: string;
  size_bytes: number;
  created_at: number;
}

interface Props {
  instanceName: string;
  onOpenFolder: () => void;
}

export function WorldsManager({ instanceName, onOpenFolder }: Props) {
  const liquidGlass = useThemeStore((s) => s.liquidGlass);
  const [saves, setSaves] = useState<SaveEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [renameTarget, setRenameTarget] = useState<SaveEntry | null>(null);
  const [renameValue, setRenameValue] = useState('');
  const [copyTarget, setCopyTarget] = useState<SaveEntry | null>(null);
  const [copyValue, setCopyValue] = useState('');
  const [deleteTarget, setDeleteTarget] = useState<SaveEntry | null>(null);
  const [backuping, setBackuping] = useState<string | null>(null);
  const [restoreTarget, setRestoreTarget] = useState<SaveEntry | null>(null);
  const [backups, setBackups] = useState<BackupInfo[]>([]);
  const [backupsLoading, setBackupsLoading] = useState(false);
  const [selectedBackup, setSelectedBackup] = useState<BackupInfo | null>(null);
  const [asNewOpen, setAsNewOpen] = useState(false);
  const [asNewName, setAsNewName] = useState('');
  const [replaceConfirm, setReplaceConfirm] = useState(false);
  const [restoring, setRestoring] = useState(false);
  const [deleteBackup, setDeleteBackup] = useState<BackupInfo | null>(null);

  const loadSaves = useCallback(async () => {
    setLoading(true);
    try {
      const s = await invoke<SaveEntry[]>('cmd_list_saves', { instanceName });
      setSaves(s);
    } catch (e: any) {
      addToast(t('worlds.load_error', { error: e.toString() }), 'error');
    }
    setLoading(false);
  }, [instanceName]);

  useEffect(() => { loadSaves(); }, [loadSaves]);

  const formatDate = (ts: number | null) => {
    if (!ts) return '';
    return new Date(ts * 1000).toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' });
  };

  const formatSize = (bytes: number) => fmtSize(bytes);

  const gameModeLabel = (mode: string | null) => {
    if (!mode) return t('worlds.game_unknown');
    switch (mode) {
      case 'Survival': return t('worlds.game_survival');
      case 'Creative': return t('worlds.game_creative');
      case 'Adventure': return t('worlds.game_adventure');
      case 'Hardcore': return t('worlds.game_hardcore');
      default: return mode;
    }
  };

  const handleRename = async () => {
    if (!renameTarget || !renameValue.trim()) return;
    if (renameValue.trim() === renameTarget.name) { setRenameTarget(null); return; }
    try {
      await invoke('cmd_rename_world', { instanceName, oldName: renameTarget.name, newName: renameValue.trim() });
      addToast(t('worlds.rename_toast', { name: renameValue.trim() }), 'success');
      setRenameTarget(null);
      loadSaves();
    } catch (e: any) {
      addToast(t('worlds.action_error', { error: e.toString() }), 'error');
    }
  };

  const handleCopy = async () => {
    if (!copyTarget || !copyValue.trim()) return;
    try {
      await invoke('cmd_copy_world', { instanceName, worldName: copyTarget.name, newName: copyValue.trim() });
      addToast(t('worlds.copy_toast', { name: copyValue.trim() }), 'success');
      setCopyTarget(null);
      loadSaves();
    } catch (e: any) {
      addToast(t('worlds.action_error', { error: e.toString() }), 'error');
    }
  };

  const handleDelete = async () => {
    if (!deleteTarget) return;
    try {
      await invoke('cmd_delete_world', { instanceName, worldName: deleteTarget.name });
      addToast(t('worlds.delete_toast'), 'success');
      setDeleteTarget(null);
      loadSaves();
    } catch (e: any) {
      addToast(t('worlds.action_error', { error: e.toString() }), 'error');
    }
  };

  const handleBackup = async (save: SaveEntry) => {
    setBackuping(save.name);
    try {
      await invoke('cmd_backup_world', { instanceName, worldName: save.name });
      addToast(t('worlds.backup_toast', { name: save.name }), 'success');
    } catch (e: any) {
      addToast(t('worlds.backup_error', { error: e.toString() }), 'error');
    }
    setBackuping(null);
  };

  const loadBackups = useCallback(async (worldName: string) => {
    setBackupsLoading(true);
    try {
      const b = await invoke<BackupInfo[]>('cmd_list_world_backups', { instanceName, worldName });
      setBackups(b);
      setSelectedBackup(b[0] ?? null);
    } catch (e: any) {
      addToast(t('worlds.action_error', { error: e.toString() }), 'error');
    }
    setBackupsLoading(false);
  }, [instanceName]);

  const openRestore = (save: SaveEntry) => {
    setRestoreTarget(save);
    setBackups([]);
    setSelectedBackup(null);
    setAsNewOpen(false);
    setReplaceConfirm(false);
    loadBackups(save.name);
  };

  const doRestore = async (mode: 'replace' | 'as_new', newName?: string) => {
    if (!restoreTarget || !selectedBackup) return;
    setRestoring(true);
    try {
      await invoke('cmd_restore_world', {
        instanceName,
        worldName: restoreTarget.name,
        backupFilename: selectedBackup.filename,
        mode,
        newName: newName ?? null,
      });
      if (mode === 'replace') {
        addToast(t('worlds.restore_toast'), 'success');
      } else {
        addToast(t('worlds.restore_as_new_toast', { name: newName ?? '' }), 'success');
      }
      setRestoreTarget(null);
      setAsNewOpen(false);
      setReplaceConfirm(false);
      loadSaves();
    } catch (e: any) {
      addToast(t('worlds.restore_error', { error: e.toString() }), 'error');
    }
    setRestoring(false);
  };

  const handleDeleteBackup = async () => {
    if (!restoreTarget || !deleteBackup) return;
    try {
      await invoke('cmd_delete_world_backup', {
        instanceName,
        worldName: restoreTarget.name,
        backupFilename: deleteBackup.filename,
      });
      addToast(t('worlds.backup_delete_toast'), 'success');
      setDeleteBackup(null);
      loadBackups(restoreTarget.name);
    } catch (e: any) {
      addToast(t('worlds.action_error', { error: e.toString() }), 'error');
    }
  };

  return (
    /* Liquid Glass: a material card per world and no section-wide tray
       behind them (owner: «в разделе с мирами сделай такие же карточки как
       и с серверами. и заднее стекло убери»). Normal themes keep the
       original flat list. */
    <div className={`content-manager__pane${liquidGlass ? ' content-manager__pane--glassless' : ''}`} style={{ flex: 1, display: 'flex', flexDirection: 'column', overflow: 'hidden' }}>
      <div style={{ padding: 'var(--space-md) var(--space-2xl)', display: 'flex', alignItems: 'center', gap: 'var(--space-md)', borderBottom: '1px solid var(--surface-border)', flexShrink: 0 }}>
        <h2 style={{ fontSize: 'var(--font-size-lg)', fontWeight: 600, margin: 0, flex: 1 }}>
          {t('worlds.title')}
          <span style={{ fontWeight: 400, color: 'var(--text-tertiary)', fontSize: 'var(--font-size-sm)', marginLeft: 8 }}>
            {t('worlds.count', { n: saves.length.toString() })}
          </span>
        </h2>
        <Button size="sm" variant="ghost" onClick={loadSaves} disabled={loading}>
          <RefreshCw size={14} className={loading ? 'icon-spin' : undefined} /> {t('common.refresh')}
        </Button>
      </div>

      <div style={{ flex: 1, overflowY: 'auto', ...(liquidGlass ? { padding: 'var(--space-lg) var(--space-2xl)' } : {}) }}>
        {loading ? (
          <div style={{ padding: 40, textAlign: 'center', color: 'var(--text-tertiary)' }}>{t('common.loading')}</div>
        ) : saves.length === 0 ? (
          <div style={{ padding: 40, textAlign: 'center', color: 'var(--text-secondary)' }}>
            <Globe size={32} style={{ opacity: 0.5, marginBottom: 'var(--space-sm)' }} />
            <div>{t('worlds.empty')}</div>
            <div style={{ fontSize: 'var(--font-size-sm)', marginTop: 4 }}>{t('worlds.hint')}</div>
          </div>
        ) : liquidGlass ? (
          <div className="glass-row-list" style={{ display: 'flex', flexDirection: 'column', gap: 'var(--space-sm)' }}>
            {saves.map((save) => (
              <div key={save.name} className="glass-card" style={{ padding: 'var(--space-md)', display: 'flex', alignItems: 'center', gap: 'var(--space-md)' }}>
                {save.icon_data ? (
                  <img src={save.icon_data} alt="" style={{ width: 36, height: 36, borderRadius: 'var(--radius-sm)', objectFit: 'cover', flexShrink: 0 }} />
                ) : (
                  <div style={{ width: 36, height: 36, borderRadius: 'var(--radius-sm)', background: 'var(--surface-glass)', display: 'flex', alignItems: 'center', justifyContent: 'center', flexShrink: 0 }}>
                    <Globe size={18} style={{ color: 'var(--text-tertiary)' }} />
                  </div>
                )}
                <div style={{ flex: 1, minWidth: 0 }}>
                  <div style={{ fontWeight: 600, fontSize: 'var(--font-size-sm)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{save.name}</div>
                  <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{formatDate(save.last_modified)}</div>
                </div>
                <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-secondary)', width: 110, flexShrink: 0 }}>{gameModeLabel(save.game_mode)}</div>
                <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-secondary)', width: 80, textAlign: 'right', flexShrink: 0 }}>{formatSize(save.size_bytes)}</div>
                <div style={{ display: 'flex', gap: 'var(--space-sm)', flexShrink: 0 }}>
                  <Tooltip content={t('worlds.backup')}>
                    <Button size="sm" variant="ghost" disabled={backuping === save.name} onClick={() => handleBackup(save)} style={backuping === save.name ? { color: 'var(--accent)' } : undefined}><Archive size={14} /></Button>
                  </Tooltip>
                  <Tooltip content={t('worlds.restore')}>
                    <Button size="sm" variant="ghost" onClick={() => openRestore(save)}><RotateCcw size={14} /></Button>
                  </Tooltip>
                  <Tooltip content={t('worlds.rename')}>
                    <Button size="sm" variant="ghost" onClick={() => { setRenameTarget(save); setRenameValue(save.name); }}><Pencil size={14} /></Button>
                  </Tooltip>
                  <Tooltip content={t('worlds.copy')}>
                    <Button size="sm" variant="ghost" onClick={() => { setCopyTarget(save); setCopyValue(`${save.name} ${t('worlds.copy_suffix')}`); }}><Copy size={14} /></Button>
                  </Tooltip>
                  <Tooltip content={t('worlds.delete')}>
                    <Button size="sm" variant="ghost" onClick={() => setDeleteTarget(save)} style={{ color: 'var(--color-danger)' }}><Trash2 size={14} /></Button>
                  </Tooltip>
                  {save.seed != null && (
                    <Tooltip content={t('worlds.copy_seed')}>
                      <CopyButton
                        tooltip={t('worlds.copy_seed')}
                        text={() => String(save.seed)}
                        size={14}
                        className="btn btn--ghost btn--sm"
                      />
                    </Tooltip>
                  )}
                </div>
              </div>
            ))}
          </div>
        ) : (
          saves.map((save) => (
            <div key={save.name} style={{
              display: 'grid', gridTemplateColumns: '1fr 110px 90px 170px',
              padding: '8px var(--space-2xl)', alignItems: 'center',
              borderBottom: '1px solid var(--surface-border)', gap: 'var(--space-sm)',
            }}>
              <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-sm)', minWidth: 0 }}>
                {save.icon_data ? (
                  <img src={save.icon_data} alt="" style={{ width: 32, height: 32, borderRadius: 'var(--radius-sm)', objectFit: 'cover', flexShrink: 0 }} />
                ) : (
                  <div style={{ width: 32, height: 32, borderRadius: 'var(--radius-sm)', background: 'var(--surface-glass)', display: 'flex', alignItems: 'center', justifyContent: 'center', flexShrink: 0 }}>
                    <Globe size={16} style={{ color: 'var(--text-tertiary)' }} />
                  </div>
                )}
                <div style={{ minWidth: 0 }}>
                  <div style={{ fontWeight: 500, fontSize: 'var(--font-size-sm)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{save.name}</div>
                  <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)' }}>{formatDate(save.last_modified)}</div>
                </div>
              </div>
              <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-secondary)' }}>{gameModeLabel(save.game_mode)}</div>
              <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-secondary)', textAlign: 'right' }}>{formatSize(save.size_bytes)}</div>
              <div style={{ display: 'flex', gap: 2, justifyContent: 'flex-end' }}>
                <Tooltip content={t('worlds.backup')}>
                  <button onClick={() => handleBackup(save)} disabled={backuping === save.name} style={{ background: 'none', border: 'none', padding: 4, cursor: 'pointer', color: backuping === save.name ? 'var(--accent)' : 'var(--text-tertiary)', borderRadius: 'var(--radius-sm)' }}><Archive size={14} /></button>
                </Tooltip>
                <Tooltip content={t('worlds.restore')}>
                  <button onClick={() => openRestore(save)} style={{ background: 'none', border: 'none', padding: 4, cursor: 'pointer', color: 'var(--text-tertiary)', borderRadius: 'var(--radius-sm)' }}><RotateCcw size={14} /></button>
                </Tooltip>
                <Tooltip content={t('worlds.rename')}><button onClick={() => { setRenameTarget(save); setRenameValue(save.name); }} style={{ background: 'none', border: 'none', padding: 4, cursor: 'pointer', color: 'var(--text-tertiary)', borderRadius: 'var(--radius-sm)' }}><Pencil size={14} /></button></Tooltip>
                <Tooltip content={t('worlds.copy')}><button onClick={() => { setCopyTarget(save); setCopyValue(`${save.name} ${t('worlds.copy_suffix')}`); }} style={{ background: 'none', border: 'none', padding: 4, cursor: 'pointer', color: 'var(--text-tertiary)', borderRadius: 'var(--radius-sm)' }}><Copy size={14} /></button></Tooltip>
                <Tooltip content={t('worlds.delete')}><button onClick={() => setDeleteTarget(save)} style={{ background: 'none', border: 'none', padding: 4, cursor: 'pointer', color: 'var(--text-tertiary)', borderRadius: 'var(--radius-sm)' }}><Trash2 size={14} /></button></Tooltip>
                {save.seed != null && (
                  <Tooltip content={t('worlds.copy_seed')}>
                    <CopyButton
                      tooltip={t('worlds.copy_seed')}
                      text={() => String(save.seed)}
                      size={14}
                      className=""
                      style={{ background: 'none', border: 'none', padding: 4, cursor: 'pointer', color: 'var(--text-tertiary)', borderRadius: 'var(--radius-sm)' }}
                    />
                  </Tooltip>
                )}
              </div>
            </div>
          ))
        )}
      </div>

      <div style={{ padding: '8px var(--space-2xl)', borderTop: '1px solid var(--surface-border)', display: 'flex', gap: 'var(--space-sm)', alignItems: 'center', flexShrink: 0 }}>
        <Button size="sm" variant="ghost" onClick={onOpenFolder}>
          <FolderOpen size={14} /> {t('worlds.open_folder')}
        </Button>
      </div>

      {/* Rename modal */}
      <Modal open={!!renameTarget} onClose={() => setRenameTarget(null)} title={t('worlds.rename_title')}>
        <Input label={t('worlds.rename_label')} id="world-rename" value={renameValue} onChange={(e) => setRenameValue(e.target.value)} autoFocus />
        <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 'var(--space-sm)', marginTop: 'var(--space-lg)' }}>
          <Button variant="ghost" onClick={() => setRenameTarget(null)}>{t('common.cancel')}</Button>
          <Button onClick={handleRename} disabled={!renameValue.trim() || renameValue.trim() === renameTarget?.name}>{t('common.confirm')}</Button>
        </div>
      </Modal>

      {/* Copy modal */}
      <Modal open={!!copyTarget} onClose={() => setCopyTarget(null)} title={t('worlds.copy_title')}>
        <Input label={t('worlds.copy_label')} id="world-copy" value={copyValue} onChange={(e) => setCopyValue(e.target.value)} autoFocus />
        <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 'var(--space-sm)', marginTop: 'var(--space-lg)' }}>
          <Button variant="ghost" onClick={() => setCopyTarget(null)}>{t('common.cancel')}</Button>
          <Button onClick={handleCopy} disabled={!copyValue.trim()}>{t('common.confirm')}</Button>
        </div>
      </Modal>

      {/* Delete confirmation */}
      <Modal open={!!deleteTarget} onClose={() => setDeleteTarget(null)} title={t('worlds.delete_title')}>
        <p style={{ margin: 0, color: 'var(--text-secondary)' }}>
          {t('worlds.delete_confirm', { name: deleteTarget?.name ?? '' })}
        </p>
        <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 'var(--space-sm)', marginTop: 'var(--space-lg)' }}>
          <Button variant="ghost" onClick={() => setDeleteTarget(null)}>{t('common.cancel')}</Button>
          <Button variant="danger" onClick={handleDelete}>{t('worlds.delete')}</Button>
        </div>
      </Modal>

      {/* Restore backups modal */}
      <Modal open={!!restoreTarget} onClose={() => setRestoreTarget(null)} title={restoreTarget ? t('worlds.restore_title', { name: restoreTarget.name }) : ''} maxWidth={520}>
        {backupsLoading ? (
          <div style={{ padding: 24, textAlign: 'center', color: 'var(--text-tertiary)' }}>{t('common.loading')}</div>
        ) : backups.length === 0 ? (
          <div style={{ padding: 24, textAlign: 'center', color: 'var(--text-tertiary)' }}>
            <Archive size={28} style={{ opacity: 0.3, marginBottom: 'var(--space-sm)' }} />
            <div>{t('worlds.restore_empty')}</div>
            <div style={{ fontSize: 'var(--font-size-sm)', marginTop: 4 }}>{t('worlds.restore_hint')}</div>
          </div>
        ) : (
          <>
            <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)', marginBottom: 'var(--space-sm)' }}>
              {t('worlds.backups_count', { n: backups.length.toString() })}
            </div>
            <div style={{ maxHeight: 260, overflowY: 'auto', display: 'flex', flexDirection: 'column', gap: 4 }}>
              {backups.map((b) => (
                <div key={b.filename} onClick={() => setSelectedBackup(b)} style={{
                  display: 'flex', alignItems: 'center', gap: 'var(--space-sm)',
                  padding: '8px 10px', borderRadius: 'var(--radius-sm)', cursor: 'pointer',
                  border: selectedBackup?.filename === b.filename ? '1px solid var(--accent)' : '1px solid var(--surface-border)',
                  background: selectedBackup?.filename === b.filename ? 'var(--accent-dim, rgba(255,255,255,0.04))' : 'transparent',
                  transition: 'border-color 160ms ease, background 160ms ease',
                }}>
                  <div style={{ flex: 1, minWidth: 0 }}>
                    <div style={{ fontSize: 'var(--font-size-sm)', fontWeight: 500 }}>{formatDate(b.created_at)}</div>
                    <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)' }}>{formatSize(b.size_bytes)}</div>
                  </div>
                  <Tooltip content={t('worlds.backup_delete')}>
                    <button onClick={(e) => { e.stopPropagation(); setDeleteBackup(b); }} style={{ background: 'none', border: 'none', padding: 4, cursor: 'pointer', color: 'var(--text-tertiary)', borderRadius: 'var(--radius-sm)' }}><Trash2 size={14} /></button>
                  </Tooltip>
                </div>
              ))}
            </div>
            <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 'var(--space-sm)', marginTop: 'var(--space-lg)' }}>
              <Button variant="ghost" onClick={() => setRestoreTarget(null)}>{t('common.cancel')}</Button>
              <Button variant="ghost" disabled={!selectedBackup} onClick={() => { setAsNewName(`${restoreTarget?.name ?? ''} ${t('worlds.copy_suffix')}`.trim()); setAsNewOpen(true); }}>{t('worlds.restore_as_new')}</Button>
              <Button variant="danger" disabled={!selectedBackup} onClick={() => setReplaceConfirm(true)}>{t('worlds.restore_replace')}</Button>
            </div>
          </>
        )}
      </Modal>

      {/* Restore as new name modal */}
      <Modal open={asNewOpen} onClose={() => setAsNewOpen(false)} title={t('worlds.restore_as_new')}>
        <Input label={t('worlds.restore_as_new_label')} id="world-restore-asnew" value={asNewName} onChange={(e) => setAsNewName(e.target.value)} autoFocus />
        <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 'var(--space-sm)', marginTop: 'var(--space-lg)' }}>
          <Button variant="ghost" onClick={() => setAsNewOpen(false)}>{t('common.cancel')}</Button>
          <Button disabled={!asNewName.trim() || restoring} onClick={() => doRestore('as_new', asNewName.trim())}>{t('common.confirm')}</Button>
        </div>
      </Modal>

      {/* Replace-world confirmation */}
      <Modal open={replaceConfirm} onClose={() => setReplaceConfirm(false)} title={t('worlds.restore_replace')}>
        <p style={{ margin: 0, color: 'var(--text-secondary)' }}>
          {t('worlds.restore_replace_confirm', { name: restoreTarget?.name ?? '' })}
        </p>
        <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 'var(--space-sm)', marginTop: 'var(--space-lg)' }}>
          <Button variant="ghost" onClick={() => setReplaceConfirm(false)}>{t('common.cancel')}</Button>
          <Button variant="danger" disabled={restoring} onClick={() => doRestore('replace')}>{t('worlds.restore_replace')}</Button>
        </div>
      </Modal>

      {/* Delete backup confirmation */}
      <Modal open={!!deleteBackup} onClose={() => setDeleteBackup(null)} title={t('worlds.backup_delete')}>
        <p style={{ margin: 0, color: 'var(--text-secondary)' }}>
          {t('worlds.backup_delete_confirm', { filename: deleteBackup?.filename ?? '' })}
        </p>
        <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 'var(--space-sm)', marginTop: 'var(--space-lg)' }}>
          <Button variant="ghost" onClick={() => setDeleteBackup(null)}>{t('common.cancel')}</Button>
          <Button variant="danger" onClick={handleDeleteBackup}>{t('common.confirm')}</Button>
        </div>
      </Modal>
    </div>
  );
}

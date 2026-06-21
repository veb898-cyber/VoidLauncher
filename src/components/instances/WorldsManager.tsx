import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Globe, FolderOpen, Pencil, Copy, Trash2, KeyRound } from 'lucide-react';
import { Button } from '../ui/Button';
import { Modal } from '../ui/Modal';
import { Input } from '../ui/Input';
import { addToast } from '../ui/Toast';
import { t } from '../../lib/i18n';

interface SaveEntry {
  name: string;
  last_modified: number | null;
  size_bytes: number;
  game_mode: string | null;
  seed: number | null;
  icon_data: string | null;
}

interface Props {
  instanceName: string;
  onOpenFolder: () => void;
}

export function WorldsManager({ instanceName, onOpenFolder }: Props) {
  const [saves, setSaves] = useState<SaveEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [renameTarget, setRenameTarget] = useState<SaveEntry | null>(null);
  const [renameValue, setRenameValue] = useState('');
  const [copyTarget, setCopyTarget] = useState<SaveEntry | null>(null);
  const [copyValue, setCopyValue] = useState('');
  const [deleteTarget, setDeleteTarget] = useState<SaveEntry | null>(null);

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

  const formatSize = (bytes: number) => {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
    return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GB`;
  };

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

  const handleCopySeed = async (seed: number) => {
    try {
      await navigator.clipboard.writeText(seed.toString());
      addToast(t('worlds.seed_toast'), 'success');
    } catch (e: any) {
      addToast(t('worlds.action_error', { error: e.toString() }), 'error');
    }
  };

  return (
    <div style={{ flex: 1, display: 'flex', flexDirection: 'column', overflow: 'hidden' }}>
      <div style={{ padding: 'var(--space-md) var(--space-2xl)', display: 'flex', alignItems: 'center', gap: 'var(--space-md)', borderBottom: '1px solid var(--surface-border)', flexShrink: 0 }}>
        <h2 style={{ fontSize: 'var(--font-size-lg)', fontWeight: 600, margin: 0, flex: 1 }}>
          {t('worlds.title')}
          <span style={{ fontWeight: 400, color: 'var(--text-tertiary)', fontSize: 'var(--font-size-sm)', marginLeft: 8 }}>
            {t('worlds.count', { n: saves.length.toString() })}
          </span>
        </h2>
        <Button size="sm" variant="ghost" onClick={loadSaves}>{t('common.refresh')}</Button>
      </div>

      <div style={{ flex: 1, overflowY: 'auto' }}>
        {loading ? (
          <div style={{ padding: 40, textAlign: 'center', color: 'var(--text-tertiary)' }}>{t('common.loading')}</div>
        ) : saves.length === 0 ? (
          <div style={{ padding: 40, textAlign: 'center', color: 'var(--text-tertiary)' }}>
            <Globe size={32} style={{ opacity: 0.3, marginBottom: 'var(--space-sm)' }} />
            <div>{t('worlds.empty')}</div>
            <div style={{ fontSize: 'var(--font-size-sm)', marginTop: 4 }}>{t('worlds.hint')}</div>
          </div>
        ) : (
          saves.map((save) => (
            <div key={save.name} style={{
              display: 'grid', gridTemplateColumns: '1fr 120px 100px 80px',
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
                <button title={t('worlds.rename')} onClick={() => { setRenameTarget(save); setRenameValue(save.name); }} style={{ background: 'none', border: 'none', padding: 4, cursor: 'pointer', color: 'var(--text-tertiary)', borderRadius: 'var(--radius-sm)' }}><Pencil size={14} /></button>
                <button title={t('worlds.copy')} onClick={() => { setCopyTarget(save); setCopyValue(`${save.name} (Copy)`); }} style={{ background: 'none', border: 'none', padding: 4, cursor: 'pointer', color: 'var(--text-tertiary)', borderRadius: 'var(--radius-sm)' }}><Copy size={14} /></button>
                <button title={t('worlds.delete')} onClick={() => setDeleteTarget(save)} style={{ background: 'none', border: 'none', padding: 4, cursor: 'pointer', color: 'var(--text-tertiary)', borderRadius: 'var(--radius-sm)' }}><Trash2 size={14} /></button>
                {save.seed != null && (
                  <button title={t('worlds.copy_seed')} onClick={() => handleCopySeed(save.seed!)} style={{ background: 'none', border: 'none', padding: 4, cursor: 'pointer', color: 'var(--text-tertiary)', borderRadius: 'var(--radius-sm)' }}><KeyRound size={14} /></button>
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
    </div>
  );
}

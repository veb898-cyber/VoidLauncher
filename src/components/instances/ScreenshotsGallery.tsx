import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Camera, FolderOpen, RefreshCw, Trash2 } from 'lucide-react';
import { Button } from '../ui/Button';
import { Modal } from '../ui/Modal';
import { addToast } from '../ui/Toast';
import { t } from '../../lib/i18n';

interface ScreenshotEntry {
  filename: string;
  last_modified: number | null;
  size_bytes: number;
}

interface Props {
  instanceName: string;
  onOpenFolder: () => void;
}

export function ScreenshotsGallery({ instanceName, onOpenFolder }: Props) {
  const [screenshots, setScreenshots] = useState<ScreenshotEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [selected, setSelected] = useState<string | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<string | null>(null);

  const loadScreenshots = useCallback(async () => {
    setLoading(true);
    try {
      const s = await invoke<ScreenshotEntry[]>('cmd_list_screenshots', { instanceName });
      setScreenshots(s);
    } catch { /* ignore */ }
    setLoading(false);
  }, [instanceName]);

  useEffect(() => { loadScreenshots(); }, [loadScreenshots]);

  const handleDelete = async () => {
    if (!deleteTarget) return;
    try {
      await invoke('cmd_delete_screenshot', { instanceName, filename: deleteTarget });
      addToast(t('screenshots.deleted'), 'success');
      setDeleteTarget(null);
      setSelected(null);
      loadScreenshots();
    } catch (e: any) {
      addToast(e.toString(), 'error');
    }
  };

  return (
    <div style={{ flex: 1, display: 'flex', flexDirection: 'column', overflow: 'hidden' }}>
      <div style={{ padding: 'var(--space-md) var(--space-2xl)', display: 'flex', alignItems: 'center', gap: 'var(--space-md)', borderBottom: '1px solid var(--surface-border)', flexShrink: 0 }}>
        <h2 style={{ fontSize: 'var(--font-size-lg)', fontWeight: 600, margin: 0, flex: 1 }}>
          {t('screenshots.title')}
          <span style={{ fontWeight: 400, color: 'var(--text-tertiary)', fontSize: 'var(--font-size-sm)', marginLeft: 8 }}>
            {t('screenshots.count', { n: screenshots.length.toString() })}
          </span>
        </h2>
        <Button size="sm" variant="ghost" onClick={loadScreenshots}>
          <RefreshCw size={14} /> {t('common.refresh')}
        </Button>
      </div>

      <div style={{ flex: 1, overflowY: 'auto', padding: 'var(--space-lg) var(--space-2xl)' }}>
        {loading ? (
          <div style={{ padding: 40, textAlign: 'center', color: 'var(--text-tertiary)' }}>{t('common.loading')}</div>
        ) : screenshots.length === 0 ? (
          <div style={{ padding: 40, textAlign: 'center', color: 'var(--text-tertiary)' }}>
            <Camera size={32} style={{ opacity: 0.3, marginBottom: 'var(--space-sm)' }} />
            <div>{t('screenshots.empty')}</div>
            <div style={{ fontSize: 'var(--font-size-sm)', marginTop: 4 }}>{t('screenshots.hint')}</div>
          </div>
        ) : (
          <div className="instance-grid">
            {screenshots.map((s) => {
              const isSelected = selected === s.filename;
              return (
                <div
                  key={s.filename}
                  className="instance-card"
                  style={{
                    borderColor: isSelected ? 'var(--primary)' : undefined,
                    transform: isSelected ? 'translateY(-4px)' : undefined,
                    boxShadow: isSelected ? 'var(--shadow-lg), var(--glow-primary)' : undefined,
                  }}
                  onClick={() => setSelected(isSelected ? null : s.filename)}
                  onContextMenu={(e) => { e.preventDefault(); setDeleteTarget(s.filename); }}
                >
                  <div className="instance-card__banner" style={{ height: 160, opacity: 1, background: 'var(--bg-tertiary)', display: 'flex', alignItems: 'center', justifyContent: 'center' }}>
                    <Camera size={32} style={{ color: 'var(--text-tertiary)', opacity: 0.4 }} />
                    <button
                      onClick={(e) => { e.stopPropagation(); setDeleteTarget(s.filename); }}
                      className="btn btn--ghost btn--sm"
                      style={{
                        position: 'absolute', top: 6, right: 6, zIndex: 2,
                        background: 'rgba(0,0,0,0.5)', color: 'white',
                        padding: '4px 6px', borderRadius: 'var(--radius-sm)',
                        border: 'none', cursor: 'pointer', opacity: 0,
                        transition: 'opacity 0.15s',
                      }}
                      title={t('common.delete')}
                    ><Trash2 size={14} /></button>
                    <div className="instance-card__banner-overlay" style={{ background: 'linear-gradient(to bottom, transparent 60%, var(--bg-primary))' }}>
                    </div>
                  </div>
                  <div className="instance-card__body instance-card__body--horizontal" style={{ padding: 'var(--space-sm) var(--space-md)' }}>
                    <div className="instance-card__info" style={{ minWidth: 0 }}>
                      <div className="instance-card__name" style={{ fontSize: 'var(--font-size-xs)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                        {s.filename}
                      </div>
                    </div>
                  </div>
                </div>
              );
            })}
          </div>
        )}
      </div>

      <div style={{ padding: '8px var(--space-2xl)', borderTop: '1px solid var(--surface-border)', display: 'flex', gap: 'var(--space-sm)', alignItems: 'center', flexShrink: 0 }}>
        <Button size="sm" variant="ghost" onClick={onOpenFolder}>
          <FolderOpen size={14} /> {t('screenshots.open_folder')}
        </Button>
        <div style={{ flex: 1 }} />
        {selected && (
          <Button size="sm" variant="danger" onClick={() => setDeleteTarget(selected)}>
            <Trash2 size={14} /> {t('common.delete')}
          </Button>
        )}
      </div>

      <Modal open={!!deleteTarget} onClose={() => setDeleteTarget(null)} title={t('screenshots.delete_title')}>
        <p style={{ margin: 0, color: 'var(--text-secondary)' }}>
          {t('screenshots.delete_confirm', { name: deleteTarget ?? '' })}
        </p>
        <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 'var(--space-sm)', marginTop: 'var(--space-lg)' }}>
          <Button variant="ghost" onClick={() => setDeleteTarget(null)}>{t('common.cancel')}</Button>
          <Button variant="danger" onClick={handleDelete}>{t('common.delete')}</Button>
        </div>
      </Modal>
    </div>
  );
}

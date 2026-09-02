import { useState, useEffect, useCallback, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Camera, FolderOpen, RefreshCw, Trash2 } from 'lucide-react';
import { Button } from '../ui/Button';
import { Tooltip } from '../ui/Tooltip';
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

/** Lazily load a screenshot's data URL once, when the tile scrolls into view. */
function ScreenshotTile({ instanceName, filename, delay }: { instanceName: string; filename: string; delay: number }) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const [src, setSrc] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    const node = containerRef.current;
    if (!node) return;
    const observer = new IntersectionObserver((entries) => {
      for (const entry of entries) {
        if (entry.isIntersecting) {
          observer.disconnect();
          invoke<string>('cmd_read_screenshot', { instanceName, filename })
            .then((dataUrl) => setSrc(dataUrl))
            .catch(() => setFailed(true));
        }
      }
    }, { rootMargin: '200px' });
    observer.observe(node);
    return () => observer.disconnect();
  }, [instanceName, filename]);

  return (
    <div ref={containerRef} className="instance-card__banner" style={{ height: 160, overflow: 'hidden', background: 'var(--bg-tertiary)', animationDelay: `${delay * 24}ms` }}>
      {src ? (
        <img
          src={src}
          alt={filename}
          loading="lazy"
          style={{ width: '100%', height: '100%', objectFit: 'cover', display: 'block' }}
        />
      ) : failed ? (
        <div style={{ height: '100%', display: 'flex', alignItems: 'center', justifyContent: 'center' }}>
          <Camera size={32} style={{ color: 'var(--text-tertiary)', opacity: 0.4 }} />
        </div>
      ) : (
        <div style={{ height: '100%', display: 'flex', alignItems: 'center', justifyContent: 'center' }}>
          <Camera size={32} style={{ color: 'var(--text-tertiary)', opacity: 0.4 }} />
        </div>
      )}
      <div className="instance-card__banner-overlay" style={{ background: 'linear-gradient(to bottom, transparent 60%, var(--bg-primary))' }} />
    </div>
  );
}

export function ScreenshotsGallery({ instanceName, onOpenFolder }: Props) {
  const [screenshots, setScreenshots] = useState<ScreenshotEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [viewer, setViewer] = useState<string | null>(null);
  const [viewerSrc, setViewerSrc] = useState<string | null>(null);
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
      if (viewer === deleteTarget) setViewer(null);
      loadScreenshots();
    } catch (e: any) {
      addToast(e.toString(), 'error');
    }
  };

  const openViewer = async (filename: string) => {
    setViewer(filename);
    setViewerSrc(null);
    try {
      const dataUrl = await invoke<string>('cmd_read_screenshot', { instanceName, filename });
      setViewerSrc(dataUrl);
    } catch { /* keep placeholder */ }
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
            {screenshots.map((s, i) => {
              return (
                <div
                  key={s.filename}
                  className="instance-card stagger-in"
                  style={{
                    animationDelay: `${Math.min(i, 9) * 24}ms`,
                    cursor: 'pointer',
                  }}
                  onClick={() => openViewer(s.filename)}
                  onContextMenu={(e) => { e.preventDefault(); setDeleteTarget(s.filename); }}
                >
                  <ScreenshotTile instanceName={instanceName} filename={s.filename} delay={Math.min(i, 9)} />
                  <Tooltip content={t('common.delete')}>
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
                    ><Trash2 size={14} /></button>
                  </Tooltip>
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

      <Modal open={!!viewer} onClose={() => setViewer(null)} title={viewer ?? ''}>
        <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'center', maxHeight: '70vh' }}>
          {viewerSrc ? (
            <img src={viewerSrc} alt={viewer ?? ''} style={{ maxWidth: '100%', maxHeight: '70vh', objectFit: 'contain', borderRadius: 'var(--radius-sm)' }} />
          ) : (
            <div style={{ padding: 40, color: 'var(--text-tertiary)' }}>{t('common.loading')}</div>
          )}
        </div>
        <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 'var(--space-sm)', marginTop: 'var(--space-lg)' }}>
          <Button variant="danger" onClick={() => { setViewer(null); setDeleteTarget(viewer); }}>
            <Trash2 size={14} /> {t('common.delete')}
          </Button>
        </div>
      </Modal>
    </div>
  );
}

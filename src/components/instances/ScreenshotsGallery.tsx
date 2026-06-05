import { useState, useEffect, useCallback } from 'react';
import { invoke, convertFileSrc } from '@tauri-apps/api/core';
import { Camera, FolderOpen } from 'lucide-react';
import { Button } from '../ui/Button';
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

  const loadScreenshots = useCallback(async () => {
    setLoading(true);
    try {
      const s = await invoke<ScreenshotEntry[]>('cmd_list_screenshots', { instanceName });
      setScreenshots(s);
    } catch { /* ignore */ }
    setLoading(false);
  }, [instanceName]);

  // Resolve the screenshot directory once on mount so convertFileSrc can
  // produce a properly-encoded asset URL per image. Tauri's `convertFileSrc`
  // handles path separators and special characters (spaces, #, &, +, etc.)
  // for both Windows and POSIX paths.
  const [screenshotDir, setScreenshotDir] = useState<string | null>(null);
  useEffect(() => {
    let cancelled = false;
    invoke<string>('cmd_get_instance_dir', { instanceName })
      .then((dir) => { if (!cancelled) setScreenshotDir(`${dir}/screenshots`); })
      .catch(() => { /* ignore — fall back to nothing */ });
    return () => { cancelled = true; };
  }, [instanceName]);

  useEffect(() => { loadScreenshots(); }, [loadScreenshots]);

  const formatDate = (ts: number | null) => {
    if (!ts) return '';
    return new Date(ts * 1000).toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' });
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
        <Button size="sm" variant="ghost" onClick={() => { loadScreenshots(); }}>
          {t('common.refresh')}
        </Button>
      </div>

      <div style={{ flex: 1, overflowY: 'auto', padding: 'var(--space-md)' }}>
        {loading ? (
          <div style={{ padding: 40, textAlign: 'center', color: 'var(--text-tertiary)' }}>{t('common.loading')}</div>
        ) : screenshots.length === 0 ? (
          <div style={{ padding: 40, textAlign: 'center', color: 'var(--text-tertiary)' }}>
            <Camera size={32} style={{ opacity: 0.3, marginBottom: 'var(--space-sm)' }} />
            <div>{t('screenshots.empty')}</div>
            <div style={{ fontSize: 'var(--font-size-sm)', marginTop: 4 }}>{t('screenshots.hint')}</div>
          </div>
        ) : (
          <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fill, minmax(240px, 1fr))', gap: 'var(--space-sm)' }}>
            {screenshots.map((s) => (
              <div
                key={s.filename}
                onClick={() => setSelected(selected === s.filename ? null : s.filename)}
                style={{
                  borderRadius: 'var(--radius-md)', overflow: 'hidden', cursor: 'pointer',
                  border: selected === s.filename ? '2px solid var(--primary)' : '2px solid transparent',
                  background: 'var(--surface-glass)',
                  transition: 'all 0.15s',
                }}
              >
                <img
                  src={screenshotDir ? convertFileSrc(`${screenshotDir}/${s.filename}`) : ''}
                  alt={s.filename}
                  loading="lazy"
                  style={{ width: '100%', height: 140, objectFit: 'cover', display: 'block' }}
                  onError={(e) => { e.currentTarget.style.display = 'none'; }}
                />
                <div style={{ padding: '6px 8px', fontSize: 'var(--font-size-xs)', color: 'var(--text-secondary)' }}>
                  {formatDate(s.last_modified)}
                </div>
              </div>
            ))}
          </div>
        )}
      </div>

      <div style={{ padding: '8px var(--space-2xl)', borderTop: '1px solid var(--surface-border)', display: 'flex', gap: 'var(--space-sm)', alignItems: 'center', flexShrink: 0 }}>
        <Button size="sm" variant="ghost" onClick={onOpenFolder}>
          <FolderOpen size={14} /> {t('screenshots.open_folder')}
        </Button>
      </div>
    </div>
  );
}
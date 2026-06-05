import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Globe, FolderOpen } from 'lucide-react';
import { Button } from '../ui/Button';
import { addToast } from '../ui/Toast';
import { t } from '../../lib/i18n';

interface SaveEntry {
  name: string;
  last_modified: number | null;
  size_bytes: number;
}

interface Props {
  instanceName: string;
  onOpenFolder: () => void;
}

export function WorldsManager({ instanceName, onOpenFolder }: Props) {
  const [saves, setSaves] = useState<SaveEntry[]>([]);
  const [loading, setLoading] = useState(true);

  const loadSaves = useCallback(async () => {
    setLoading(true);
    try {
      const s = await invoke<SaveEntry[]>('cmd_list_saves', { instanceName });
      setSaves(s);
    } catch (e: any) {
      addToast(`Failed to load worlds: ${e.toString()}`, 'error');
    }
    setLoading(false);
  }, [instanceName]);

  useEffect(() => { loadSaves(); }, [loadSaves]);

  const formatDate = (ts: number | null) => {
    if (!ts) return 'Unknown';
    return new Date(ts * 1000).toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' });
  };

  const formatSize = (bytes: number) => {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
    return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GB`;
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
        <Button size="sm" variant="ghost" onClick={() => { loadSaves(); }}>
          {t('common.refresh')}
        </Button>
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
              display: 'grid', gridTemplateColumns: '1fr 160px 120px',
              padding: '8px var(--space-2xl)', alignItems: 'center',
              borderBottom: '1px solid var(--surface-border)',
            }}>
              <div>
                <div style={{ fontWeight: 500, fontSize: 'var(--font-size-sm)' }}>{save.name}</div>
                <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)' }}>{t('worlds.last_played', { date: formatDate(save.last_modified) })}</div>
              </div>
              <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-secondary)', textAlign: 'right' }}>
                {formatSize(save.size_bytes)}
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
    </div>
  );
}
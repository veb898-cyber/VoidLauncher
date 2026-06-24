import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Package, FolderOpen, Download, Search, Check, RefreshCw, Trash2 } from 'lucide-react';
import { Button } from '../ui/Button';
import { addToast } from '../ui/Toast';
import { PackBrowser } from './PackBrowser';
import { t } from '../../lib/i18n';

interface PackEntry {
  filename: string;
  name: string;
  is_dir: boolean;
  file_size: number;
  icon: string | null;
}

interface Props {
  instanceName: string;
  packType: 'resourcepacks' | 'shaderpacks';
  onOpenFolder: () => void;
}

export function PacksManager({ instanceName, packType, onOpenFolder }: Props) {
  const [packs, setPacks] = useState<PackEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [showBrowser, setShowBrowser] = useState(false);
  const [search, setSearch] = useState('');
  const [selectedIdx, setSelectedIdx] = useState<number | null>(null);
  const [contextMenu, setContextMenu] = useState<{ x: number; y: number; idx: number } | null>(null);

  const label = packType === 'resourcepacks' ? t('content.type_resourcepack') : t('content.type_shader');

  const loadPacks = useCallback(async () => {
    setLoading(true);
    try {
      const p = await invoke<PackEntry[]>('cmd_list_packs', { instanceName, packType });
      setPacks(p);
    } catch (e: any) {
      addToast(t('packs.load_error', { label, error: e.toString() }), 'error');
    }
    setLoading(false);
  }, [instanceName, packType]);

  useEffect(() => { loadPacks(); }, [loadPacks]);

  useEffect(() => {
    const handler = () => setContextMenu(null);
    window.addEventListener('click', handler);
    return () => window.removeEventListener('click', handler);
  }, []);

  const formatSize = (bytes: number) => {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  };

  const isDisabled = (filename: string) => filename.endsWith('.disabled');

  const togglePackEnabled = async (pack: PackEntry) => {
    try {
      const dir = await invoke<string>('cmd_get_instance_dir', { instanceName });
      const packDir = `${dir}/${packType}`;
      if (isDisabled(pack.filename)) {
        const baseName = pack.filename.replace(/\.disabled$/, '');
        await invoke('cmd_rename_file', { from: `${packDir}/${pack.filename}`, to: `${packDir}/${baseName}` });
      } else {
        await invoke('cmd_rename_file', { from: `${packDir}/${pack.filename}`, to: `${packDir}/${pack.filename}.disabled` });
      }
      loadPacks();
    } catch (e: any) { addToast(t('packs.toggle_error', { error: e.toString() }), 'error'); }
  };

  const handleRemove = async (filename: string) => {
    try {
      const dir = await invoke<string>('cmd_get_instance_dir', { instanceName });
      const packPath = `${dir}/${packType}/${filename}`;
      await invoke('cmd_delete_file', { path: packPath });
      // Also remove sidecar metadata file
      const sidecarPath = `${dir}/${packType}/${filename}.voidlauncher.json`;
      try { await invoke('cmd_delete_file', { path: sidecarPath }); } catch { }
      addToast(t('packs.deleted_toast', { name: filename }), 'success');
      loadPacks();
    } catch (e: any) { addToast(t('packs.delete_error', { error: e.toString() }), 'error'); }
  };

  const filtered = packs.filter((p) => p.name.toLowerCase().includes(search.toLowerCase()) || p.filename.toLowerCase().includes(search.toLowerCase()));

  if (showBrowser) {
    return (
      <PackBrowser
        instanceName={instanceName}
        packType={packType}
        onClose={() => setShowBrowser(false)}
        onInstalled={() => { loadPacks(); }}
      />
    );
  }

  return (
    <div style={{ flex: 1, display: 'flex', flexDirection: 'column', overflow: 'hidden' }}>
      {/* Header */}
      <div style={{ padding: 'var(--space-md) var(--space-2xl)', display: 'flex', alignItems: 'center', gap: 'var(--space-md)', borderBottom: '1px solid var(--surface-border)', flexShrink: 0 }}>
        <h2 style={{ fontSize: 'var(--font-size-lg)', fontWeight: 600, margin: 0, flex: 1 }}>
          {label}
          <span style={{ fontWeight: 400, color: 'var(--text-tertiary)', fontSize: 'var(--font-size-sm)', marginLeft: 8 }}>
            {t('packs.installed_count', { n: packs.length.toString() })}
          </span>
        </h2>
        <Button size="sm" variant="ghost" onClick={() => loadPacks()}>
          <RefreshCw size={14} /> {t('common.refresh')}
        </Button>
      </div>

      {/* Grid header */}
      <div style={{
        display: 'grid', gridTemplateColumns: '36px 36px 1fr 100px',
        padding: '8px var(--space-2xl)', borderBottom: '1px solid var(--surface-border)',
        fontSize: 'var(--font-size-xs)', fontWeight: 600, color: 'var(--text-tertiary)',
        background: 'var(--bg-primary)', flexShrink: 0,
      }}>
        <div style={{ textAlign: 'center' }}>{t('manager.column_on')}</div>
        <div></div>
        <div>{t('manager.column_name')}</div>
        <div style={{ textAlign: 'right' }}>{t('manager.column_size')}</div>
      </div>

      {/* List */}
      <div style={{ flex: 1, overflowY: 'auto' }}>
        {loading ? (
          <div style={{ padding: 'var(--space-md) var(--space-2xl)' }}>
            {[1, 2, 3].map((i) => (
              <div key={i} style={{ display: 'grid', gridTemplateColumns: '36px 36px 1fr 100px', padding: '7px var(--space-2xl)', alignItems: 'center', borderBottom: '1px solid var(--surface-border)' }}>
                <div style={{ width: 16, height: 16, borderRadius: 3, background: 'var(--surface-glass)', justifySelf: 'center' }} />
                <div style={{ width: 28, height: 28, borderRadius: 4, background: 'var(--surface-glass)', justifySelf: 'center' }} />
                <div style={{ width: '60%', height: 14, borderRadius: 4, background: 'var(--surface-glass)' }} />
                <div style={{ width: 60, height: 14, borderRadius: 4, background: 'var(--surface-glass)', justifySelf: 'end' }} />
              </div>
            ))}
          </div>
        ) : filtered.length === 0 ? (
          <div style={{ padding: 40, textAlign: 'center', color: 'var(--text-tertiary)', fontSize: 'var(--font-size-sm)' }}>
            <Package size={32} style={{ opacity: 0.3, marginBottom: 'var(--space-sm)' }} />
            <div>{packs.length === 0 ? t('manager.empty', { label: label.toLowerCase() }) : t('manager.empty_search')}</div>
            {packs.length === 0 && <div style={{ marginTop: 4 }}>{t('packs.empty_helper')}</div>}
          </div>
        ) : (
          filtered.map((pack, idx) => {
            const disabled = isDisabled(pack.filename);
            const isSelected = selectedIdx === idx;
            return (
              <div key={pack.filename}
                onClick={() => setSelectedIdx(isSelected ? null : idx)}
                onContextMenu={(e) => { e.preventDefault(); setContextMenu({ x: e.clientX, y: e.clientY, idx }); }}
                style={{
                  display: 'grid', gridTemplateColumns: '36px 36px 1fr 100px',
                  padding: '6px var(--space-2xl)', alignItems: 'center', cursor: 'pointer',
                  borderBottom: '1px solid var(--surface-border)',
                  background: isSelected ? 'var(--primary-dim)' : idx % 2 === 0 ? 'transparent' : 'hsla(0, 0%, 100%, 0.02)',
                  opacity: disabled ? 0.45 : 1,
                  transition: 'background 0.12s',
                }}
              >
                <div style={{ display: 'flex', justifyContent: 'center' }}>
                  <div onClick={(e) => { e.stopPropagation(); togglePackEnabled(pack); }}
                    style={{ width: 16, height: 16, borderRadius: 3, cursor: 'pointer', border: '1.5px solid ' + (disabled ? 'var(--text-tertiary)' : 'var(--primary)'), background: disabled ? 'transparent' : 'var(--primary)', display: 'flex', alignItems: 'center', justifyContent: 'center', transition: 'all 0.15s' }}>
                    {!disabled && <Check size={10} color="white" strokeWidth={3} />}
                  </div>
                </div>
                <div style={{ display: 'flex', justifyContent: 'center' }}>
                  {pack.icon ? (
                    <img src={pack.icon} alt="" style={{ width: 28, height: 28, borderRadius: 4, objectFit: 'cover' }} />
                  ) : (
                    <div style={{ width: 28, height: 28, borderRadius: 4, background: 'var(--surface-glass)', display: 'flex', alignItems: 'center', justifyContent: 'center', fontSize: 10, color: 'var(--text-tertiary)' }}>
                      {pack.name.charAt(0)}
                    </div>
                  )}
                </div>
                <div style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', fontSize: 'var(--font-size-sm)', fontWeight: 500, paddingRight: 8 }}>
                  {pack.name}
                </div>
                <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-secondary)', textAlign: 'right' }}>
                  {formatSize(pack.file_size)}
                </div>
              </div>
            );
          })
        )}
      </div>

      {/* Bottom bar */}
      <div style={{ padding: '8px var(--space-2xl)', borderTop: '1px solid var(--surface-border)', display: 'flex', gap: 'var(--space-sm)', alignItems: 'center', flexShrink: 0 }}>
        <div style={{ position: 'relative', flex: 1, maxWidth: 300 }}>
          <Search size={14} style={{ position: 'absolute', left: 10, top: '50%', transform: 'translateY(-50%)', color: 'var(--text-tertiary)' }} />
          <input className="input" type="text" placeholder={t('manager.search_placeholder', { label: label.toLowerCase() })} value={search} onChange={(e) => setSearch(e.target.value)} style={{ paddingLeft: 32, fontSize: 'var(--font-size-sm)' }} />
        </div>
        <Button size="sm" variant="secondary" onClick={() => setShowBrowser(true)}>
          <Download size={14} /> {t('common.download')}
        </Button>
        <Button size="sm" variant="ghost" onClick={onOpenFolder}>
          <FolderOpen size={14} /> {t('common.open_folder')}
        </Button>
      </div>

      {/* Context menu */}
      {contextMenu && (
        <div style={{ position: 'fixed', left: contextMenu.x, top: contextMenu.y, zIndex: 9999, background: 'var(--surface-elevated)', border: '1px solid var(--surface-border)', borderRadius: 'var(--radius-md)', padding: 4, minWidth: 160, boxShadow: '0 8px 24px rgba(0,0,0,0.4)' }}>
          <button style={{ display: 'block', width: '100%', padding: '6px 12px', border: 'none', background: 'none', color: 'var(--text-primary)', fontSize: 'var(--font-size-sm)', textAlign: 'left', cursor: 'pointer', borderRadius: 'var(--radius-sm)' }}
            onMouseEnter={(e) => e.currentTarget.style.background = 'var(--bg-secondary)'}
            onMouseLeave={(e) => e.currentTarget.style.background = 'none'}
            onClick={() => { const p = filtered[contextMenu.idx]; if (p) togglePackEnabled(p); setContextMenu(null); }}>
            {isDisabled(filtered[contextMenu.idx]?.filename || '') ? t('common.enable') : t('common.disable')}
          </button>
          <button style={{ display: 'block', width: '100%', padding: '6px 12px', border: 'none', background: 'none', color: 'var(--color-danger)', fontSize: 'var(--font-size-sm)', textAlign: 'left', cursor: 'pointer', borderRadius: 'var(--radius-sm)' }}
            onMouseEnter={(e) => e.currentTarget.style.background = 'var(--bg-secondary)'}
            onMouseLeave={(e) => e.currentTarget.style.background = 'none'}
            onClick={() => { const p = filtered[contextMenu.idx]; if (p) handleRemove(p.filename); setContextMenu(null); }}>
            <Trash2 size={12} style={{ marginRight: 6 }} /> {t('common.remove')}
          </button>
        </div>
      )}
    </div>
  );
}

import { useState } from 'react';
import { useT } from '../../lib/i18n';
import { invoke } from '@tauri-apps/api/core';
import { open as openFileDialog } from '@tauri-apps/plugin-dialog';
import { Play, Trash2, Package, Settings, Copy, Palette } from 'lucide-react';
import { useInstanceStore } from '../../stores/instanceStore';
import { Button } from '../ui/Button';
import { addToast } from '../ui/Toast';
import { InstanceEditor } from './InstanceEditor';
import { ContentManager } from './ContentManager';
import { WorldsManager } from './WorldsManager';
import { ScreenshotsGallery } from './ScreenshotsGallery';

type Tab = 'mods' | 'resourcepacks' | 'shaderpacks' | 'worlds' | 'screenshots';

interface InstanceDetailProps {
  onNavigate?: (page: string) => void;
}

export function InstanceDetail({ onNavigate: _onNavigate }: InstanceDetailProps) {
  const t = useT();
  const instances = useInstanceStore((s) => s.instances);
  const selectedInstance = useInstanceStore((s) => s.selectedInstance);
  const isLaunching = useInstanceStore((s) => s.isLaunching);
  const launchGame = useInstanceStore((s) => s.launchGame);
  const deleteInstance = useInstanceStore((s) => s.deleteInstance);
  const instance = instances.find((i) => i.name === selectedInstance);
  const [tab, setTab] = useState<Tab>('mods');
  const [showEditor, setShowEditor] = useState(false);
  const [showDeleteConfirm, setShowDeleteConfirm] = useState(false);

  const TABS: { id: Tab; label: string }[] = [
    { id: 'mods', label: t('instance_detail.tab_mods') },
    { id: 'resourcepacks', label: t('instance_detail.tab_resourcepacks') },
    { id: 'shaderpacks', label: t('instance_detail.tab_shaderpacks') },
    { id: 'worlds', label: t('instance_detail.tab_worlds') },
    { id: 'screenshots', label: t('instance_detail.tab_screenshots') },
  ];

  if (!instance) {
    return (
      <div style={{ flex: 1, display: 'flex', flexDirection: 'column', alignItems: 'center', justifyContent: 'center', color: 'var(--text-tertiary)', gap: 'var(--space-md)', padding: 'var(--space-3xl)' }}>
        <Package size={48} opacity={0.3} />
        <div style={{ fontSize: 'var(--font-size-lg)', fontWeight: 500 }}>{t('instance_detail.empty_heading')}</div>
        <div style={{ fontSize: 'var(--font-size-sm)' }}>{t('instance_detail.empty_desc')}</div>
      </div>
    );
  }

  const handleOpenFolder = async (subdir: string) => {
    try {
      await invoke('cmd_open_instance_folder', { instanceName: instance.name, subfolder: subdir });
    } catch (e: any) { addToast(t('instance_detail.folder_error', { error: e.toString() }), 'error'); }
  };

  const handleDuplicate = async () => {
    const newName = prompt(t('instance_detail.duplicate_prompt'), `${instance.name} (copy)`);
    if (!newName || newName === instance.name) return;
    try {
      await invoke('cmd_duplicate_instance', { name: instance.name, newName });
      addToast(t('instance_detail.duplicated_toast', { name: newName }), 'success');
      useInstanceStore.getState().loadInstances();
    } catch (e: any) { addToast(t('instance_detail.duplicate_error', { error: e.toString() }), 'error'); }
  };

  const handlePickIcon = async () => {
    const selected = await openFileDialog({
      title: t('instance_detail.icon_file_title'),
      filters: [{ name: 'Images', extensions: ['png', 'jpg', 'jpeg', 'ico'] }],
      multiple: false,
    });
    if (!selected) return;
    try {
      const { readFile } = await import('@tauri-apps/plugin-fs');
      const bytes = await readFile(selected);
      const ext = selected.split('.').pop()?.toLowerCase() || 'png';
      const mime = ext === 'jpg' || ext === 'jpeg' ? 'image/jpeg' : ext === 'ico' ? 'image/x-icon' : 'image/png';
      let binary = '';
      for (let i = 0; i < bytes.length; i++) binary += String.fromCharCode(bytes[i]);
      const dataUrl = `data:${mime};base64,${btoa(binary)}`;
      await invoke('cmd_set_instance_icon', { instanceName: instance.name, iconData: dataUrl });
      addToast(t('instance_detail.icon_updated'), 'success');
      useInstanceStore.getState().loadInstances();
    } catch (e: any) { addToast(t('instance_detail.icon_error', { error: e.toString() }), 'error'); }
  };

  return (
    <div style={{ flex: 1, display: 'flex', flexDirection: 'column', overflow: 'hidden' }}>
      {/* Top bar */}
      <div style={{ padding: 'var(--space-md) var(--space-2xl)', borderBottom: '1px solid var(--surface-border)', display: 'flex', alignItems: 'center', gap: 'var(--space-lg)', flexShrink: 0 }}>
          <div onClick={handlePickIcon} style={{ width: 40, height: 40, borderRadius: 'var(--radius-md)', overflow: 'hidden', cursor: 'pointer', flexShrink: 0, position: 'relative' }} title={t('instance_detail.change_icon')}>
          {instance.icon ? (
            <img src={instance.icon} alt="" style={{ width: '100%', height: '100%', objectFit: 'cover' }} />
          ) : (
            <div style={{ width: '100%', height: '100%', background: 'var(--gradient-primary)', display: 'flex', alignItems: 'center', justifyContent: 'center', fontSize: 18, fontWeight: 700, color: 'white' }}>
              {instance.name.charAt(0).toUpperCase()}
            </div>
          )}
          <div style={{ position: 'absolute', inset: 0, background: 'rgba(0,0,0,0.4)', display: 'flex', alignItems: 'center', justifyContent: 'center', opacity: 0, transition: 'opacity 0.15s' }}
            className="hover-reveal">
            <Palette size={16} />
          </div>
        </div>
        <div style={{ flex: 1, minWidth: 0 }}>
          <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-md)', marginBottom: 2, flexWrap: 'wrap' }}>
            <h1 style={{ fontSize: 'var(--font-size-xl)', fontWeight: 700, margin: 0 }}>{instance.name}</h1>
            <span className={`instance-card__tag instance-card__tag--${instance.loader.toLowerCase()}`}>{instance.loader}</span>
            <span style={{ color: 'var(--text-secondary)', fontSize: 'var(--font-size-sm)' }}>{instance.mc_version}</span>
            {instance.loader_version && <span style={{ color: 'var(--text-tertiary)', fontSize: 'var(--font-size-xs)' }}>{instance.loader_version}</span>}
          </div>
        </div>
        <div style={{ display: 'flex', gap: 'var(--space-sm)', flexShrink: 0 }}>
          <Button size="sm" variant="ghost" onClick={handleDuplicate} title={t('instance_detail.duplicate')}>
            <Copy size={14} />
          </Button>
          <Button size="sm" variant="ghost" onClick={() => setShowEditor(true)} title={t('instance_detail.edit_settings')}>
            <Settings size={14} />
          </Button>
          <Button size="sm" variant="ghost" onClick={() => setShowDeleteConfirm(true)} title={t('instance_detail.delete_instance')} style={{ color: 'var(--color-danger)' }}>
            <Trash2 size={14} />
          </Button>
          <Button size="sm" onClick={() => launchGame(instance.name)} disabled={isLaunching} loading={isLaunching} style={{ minWidth: 140 }}>
            <Play size={16} fill="currentColor" /> {t('instance_detail.launch_btn')}
          </Button>
        </div>
      </div>

      {/* Tab bar */}
      <div style={{ display: 'flex', borderBottom: '1px solid var(--surface-border)', flexShrink: 0, padding: '0 var(--space-xl)' }}>
        {TABS.map((tabEntry) => (
          <button
            key={tabEntry.id}
            onClick={() => setTab(tabEntry.id)}
            style={{
              padding: '10px 16px', border: 'none', background: 'none', cursor: 'pointer',
              color: tab === tabEntry.id ? 'var(--primary)' : 'var(--text-secondary)',
              borderBottom: tab === tabEntry.id ? '2px solid var(--primary)' : '2px solid transparent',
              fontWeight: tab === tabEntry.id ? 600 : 400, fontSize: 'var(--font-size-sm)',
              transition: 'all 0.15s',
            }}
          >
            {tabEntry.label}
          </button>
        ))}
      </div>

      {/* Tab content */}
      <div style={{ flex: 1, display: 'flex', overflow: 'hidden' }}>
        {(tab === 'mods' || tab === 'resourcepacks' || tab === 'shaderpacks') && (
          <ContentManager
            key={`${instance.name}-${tab}`}
            instanceName={instance.name}
            contentType={tab === 'mods' ? 'mod' : tab === 'resourcepacks' ? 'resourcepack' : 'shader'}
            mcVersion={instance.mc_version}
            loader={instance.loader}
            onOpenFolder={() => handleOpenFolder(tab === 'mods' ? 'mods' : tab)}
          />
        )}
        {tab === 'worlds' && (
          <WorldsManager instanceName={instance.name} onOpenFolder={() => handleOpenFolder('saves')} />
        )}
        {tab === 'screenshots' && (
          <ScreenshotsGallery instanceName={instance.name} onOpenFolder={() => handleOpenFolder('screenshots')} />
        )}
      </div>

      {/* Instance Editor Modal */}
      <InstanceEditor
        open={showEditor}
        instance={instance}
        onClose={() => setShowEditor(false)}
        onSaved={() => {
          useInstanceStore.getState().loadInstances();
          setShowEditor(false);
        }}
      />

      {/* Delete Confirmation */}
      {showDeleteConfirm && (
        <div style={{ position: 'fixed', inset: 0, background: 'rgba(0,0,0,0.6)', display: 'flex', alignItems: 'center', justifyContent: 'center', zIndex: 9999 }}>
          <div className="glass-card" style={{ padding: 'var(--space-xl)', maxWidth: 400, width: '90%' }}>
            <h3 style={{ margin: '0 0 var(--space-md)' }}>{t('instance_detail.confirm_delete_title')}</h3>
            <p style={{ color: 'var(--text-secondary)', marginBottom: 'var(--space-lg)' }}>{t('instance_detail.confirm_delete_text', { name: instance.name })}</p>
            <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 'var(--space-sm)' }}>
              <Button variant="ghost" onClick={() => setShowDeleteConfirm(false)}>{t('common.cancel')}</Button>
              <Button onClick={() => { deleteInstance(instance.name); setShowDeleteConfirm(false); }} style={{ background: 'var(--color-danger)', color: 'white' }}>{t('common.delete')}</Button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

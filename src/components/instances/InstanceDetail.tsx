import { useState, useRef, useEffect } from 'react';
import { useT } from '../../lib/i18n';
import { invoke } from '@tauri-apps/api/core';
import { open as openFileDialog } from '@tauri-apps/plugin-dialog';
import { Package, Settings, Copy, Palette, Image, Upload, Trash2 } from 'lucide-react';
import { useInstanceStore } from '../../stores/instanceStore';
import { Button } from '../ui/Button';
import { addToast } from '../ui/Toast';
import { InstanceEditor } from './InstanceEditor';
import { ContentManager } from './ContentManager';
import { WorldsManager } from './WorldsManager';
import { ScreenshotsGallery } from './ScreenshotsGallery';
import { BANNER_PRESETS, isGradientBanner } from '../../lib/bannerPresets';

type Tab = 'mods' | 'resourcepacks' | 'shaderpacks' | 'worlds' | 'screenshots';

interface InstanceDetailProps {
  onNavigate?: (page: string) => void;
}

export function InstanceDetail({ onNavigate: _onNavigate }: InstanceDetailProps) {
  const t = useT();
  const instances = useInstanceStore((s) => s.instances);
  const selectedInstance = useInstanceStore((s) => s.selectedInstance);
  const deleteInstance = useInstanceStore((s) => s.deleteInstance);
  const instance = instances.find((i) => i.name === selectedInstance);
  const [tab, setTab] = useState<Tab>('mods');
  const [showEditor, setShowEditor] = useState(false);
  const [showDeleteConfirm, setShowDeleteConfirm] = useState(false);
  const [showMenu, setShowMenu] = useState(false);
  const [showBannerPicker, setShowBannerPicker] = useState(false);
  const menuRef = useRef<HTMLDivElement>(null);
  const pickerRef = useRef<HTMLDivElement>(null);

  const TABS: { id: Tab; label: string }[] = [
    { id: 'mods', label: t('instance_detail.tab_mods') },
    { id: 'resourcepacks', label: t('instance_detail.tab_resourcepacks') },
    { id: 'shaderpacks', label: t('instance_detail.tab_shaderpacks') },
    { id: 'worlds', label: t('instance_detail.tab_worlds') },
    { id: 'screenshots', label: t('instance_detail.tab_screenshots') },
  ];

  useEffect(() => {
    if (!showMenu) return;
    const handler = (e: MouseEvent) => {
      if (menuRef.current && !menuRef.current.contains(e.target as Node)) {
        setShowMenu(false);
        setShowBannerPicker(false);
      }
    };
    document.addEventListener('mousedown', handler);
    return () => document.removeEventListener('mousedown', handler);
  }, [showMenu]);

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

  const handlePickBanner = async () => {
    const selected = await openFileDialog({
      title: t('home.change_banner'),
      filters: [{ name: 'Images', extensions: ['png', 'jpg', 'jpeg'] }],
      multiple: false,
    });
    if (!selected) return;
    try {
      const { readFile } = await import('@tauri-apps/plugin-fs');
      const bytes = await readFile(selected);
      const ext = selected.split('.').pop()?.toLowerCase() || 'png';
      const mime = ext === 'jpg' || ext === 'jpeg' ? 'image/jpeg' : 'image/png';
      let binary = '';
      for (let i = 0; i < bytes.length; i++) binary += String.fromCharCode(bytes[i]);
      const dataUrl = `data:${mime};base64,${btoa(binary)}`;
      await invoke('cmd_set_instance_banner', { instanceName: instance.name, bannerData: dataUrl });
      addToast(t('home.banner_updated'), 'success');
      useInstanceStore.getState().loadInstances();
    } catch (e: any) { addToast(e?.message || 'Failed to set banner', 'error'); }
  };

  const setBannerGradient = async (gradientId: string) => {
    try {
      await invoke('cmd_set_instance_banner', { instanceName: instance.name, bannerData: `gradient:${gradientId}` });
      setShowBannerPicker(false);
      setShowMenu(false);
      useInstanceStore.getState().loadInstances();
    } catch (e: any) {
      addToast(e?.message || 'Failed to set banner', 'error');
    }
  };

  const removeBanner = async () => {
    try {
      await invoke('cmd_set_instance_banner', { instanceName: instance.name, bannerData: '' });
      setShowBannerPicker(false);
      setShowMenu(false);
      useInstanceStore.getState().loadInstances();
    } catch (e: any) {
      addToast(e?.message || 'Failed to remove banner', 'error');
    }
  };

  return (
    <div style={{ flex: 1, display: 'flex', flexDirection: 'column', overflow: 'hidden' }}>
      {/* Top bar */}
      <div style={{ padding: 'var(--space-md) var(--space-2xl)', borderBottom: '1px solid var(--surface-border)', display: 'flex', alignItems: 'center', gap: 'var(--space-lg)', flexShrink: 0 }}>
        <div onClick={() => { setShowMenu(!showMenu); setShowBannerPicker(false); }} style={{ width: 40, height: 40, borderRadius: 'var(--radius-md)', overflow: 'hidden', cursor: 'pointer', flexShrink: 0, position: 'relative' }} title={t('instance_detail.change_icon')}>
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

        {/* Icon context menu */}
        {showMenu && !showBannerPicker && (
          <div
            ref={menuRef}
            style={{
              position: 'absolute',
              top: 52,
              left: 24,
              zIndex: 100,
              background: 'var(--bg-elevated)',
              border: '1px solid var(--surface-border)',
              borderRadius: 'var(--radius-md)',
              boxShadow: 'var(--shadow-lg)',
              padding: 'var(--space-xs)',
              minWidth: 180,
            }}
            onClick={(e) => e.stopPropagation()}
          >
            <button
              className="btn btn--ghost btn--sm"
              style={{ width: '100%', justifyContent: 'flex-start', gap: 'var(--space-sm)' }}
              onClick={() => setShowBannerPicker(true)}
            >
              <Image size={16} />
              {t('home.change_banner')}
            </button>
            <button
              className="btn btn--ghost btn--sm"
              style={{ width: '100%', justifyContent: 'flex-start', gap: 'var(--space-sm)' }}
              onClick={() => { setShowMenu(false); handlePickIcon(); }}
            >
              <Upload size={16} />
              {t('home.change_icon')}
            </button>
          </div>
        )}

        {/* Banner preset picker */}
        {showBannerPicker && (
          <div
            ref={pickerRef}
            style={{
              position: 'absolute',
              top: 52,
              left: 24,
              zIndex: 110,
              background: 'var(--bg-elevated)',
              border: '1px solid var(--surface-border)',
              borderRadius: 'var(--radius-md)',
              boxShadow: 'var(--shadow-lg)',
              padding: 'var(--space-md)',
              width: 240,
            }}
            onClick={(e) => e.stopPropagation()}
          >
            <div style={{ fontSize: 'var(--font-size-sm)', fontWeight: 600, marginBottom: 'var(--space-sm)' }}>
              {t('home.banner_presets')}
            </div>
            <div style={{ display: 'grid', gridTemplateColumns: 'repeat(4, 1fr)', gap: 'var(--space-xs)', marginBottom: 'var(--space-sm)' }}>
              {BANNER_PRESETS.map((p) => (
                <div
                  key={p.id}
                  title={p.label}
                  onClick={() => setBannerGradient(p.id)}
                  style={{
                    width: '100%',
                    aspectRatio: '16/9',
                    borderRadius: 'var(--radius-sm)',
                    background: p.gradient,
                    cursor: 'pointer',
                    border: (isGradientBanner(instance.banner ?? '') && instance.banner === `gradient:${p.id}`) ? '2px solid var(--primary)' : '2px solid transparent',
                    transition: 'border-color 0.15s',
                  }}
                />
              ))}
            </div>
            <div style={{ display: 'flex', gap: 'var(--space-xs)' }}>
              <button
                className="btn btn--ghost btn--sm"
                style={{ flex: 1, justifyContent: 'center', gap: 'var(--space-xs)' }}
                onClick={() => { setShowBannerPicker(false); setShowMenu(false); handlePickBanner(); }}
              >
                <Upload size={14} />
                {t('home.upload_image')}
              </button>
              {instance.banner && (
                <button
                  className="btn btn--ghost btn--sm"
                  style={{ justifyContent: 'center', color: 'var(--color-danger)' }}
                  onClick={removeBanner}
                  title={t('home.remove_banner')}
                >
                  x
                </button>
              )}
            </div>
          </div>
        )}

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
        </div>
      </div>

      {/* Tabs */}
      <div style={{ padding: '0 var(--space-2xl)', borderBottom: '1px solid var(--surface-border)', display: 'flex', gap: 'var(--space-md)', flexShrink: 0 }}>
        {TABS.map((tb) => (
          <button
            key={tb.id}
            className={`btn btn--tab ${tab === tb.id ? 'btn--tab-active' : ''}`}
            onClick={() => setTab(tb.id)}
          >
            {tb.label}
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

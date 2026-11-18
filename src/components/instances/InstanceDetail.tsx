import { useState, useRef, useEffect, useCallback } from 'react';
import { createPortal } from 'react-dom';
import { useT, type MessageKey } from '../../lib/i18n';
import { invoke } from '@tauri-apps/api/core';
import { open as openFileDialog } from '@tauri-apps/plugin-dialog';
import { Package, Settings, Palette, Image, Upload, Trash2, X } from 'lucide-react';
import { useInstanceStore } from '../../stores/instanceStore';
import { Button } from '../ui/Button';
import { SlidingTabs, slidingPanelId, slidingTabId } from '../ui/SlidingTabs';
import { ConfirmDialog } from '../ui/ConfirmDialog';
import { Tooltip } from '../ui/Tooltip';
import { addToast } from '../ui/Toast';
import { InstanceEditor } from './InstanceEditor';
import { ContentManager } from './ContentManager';
import { WorldsManager } from './WorldsManager';
import { ServersManager } from './ServersManager';
import { ScreenshotsGallery } from './ScreenshotsGallery';
import { BANNER_PRESETS, isGradientBanner, getGradientValue } from '../../lib/bannerPresets';
import { useBrowserGuardStore } from '../../stores/browserGuardStore';

type Tab = 'mods' | 'resourcepacks' | 'shaderpacks' | 'worlds' | 'servers' | 'screenshots';

interface InstanceDetailProps {
  onNavigate?: (page: string) => void;
}

export function InstanceDetail({ onNavigate: _onNavigate }: InstanceDetailProps) {
  const t = useT();
  const bannerLabel = (id: string) => t(`home.banner_${id.replace(/-/g, '_')}` as MessageKey);
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
  const appearanceButtonRef = useRef<HTMLButtonElement>(null);
  const returnFocusAfterPicker = useRef(false);
  const closeBannerPicker = useCallback(() => {
    // The trigger lives inside #root, which is inert while the modal is open.
    // Focus it after the inert cleanup in the effect below, not here: focusing
    // an inert subtree is silently ignored.
    returnFocusAfterPicker.current = true;
    setShowBannerPicker(false);
    setShowMenu(false);
  }, []);

  // A modal dialog must start with focus inside it, and the page behind must
  // not be reachable while it is open. The picker is portaled to <body>, so
  // `#root` is exactly the background that has to become inert.
  useEffect(() => {
    if (!showBannerPicker) return;
    const root = pickerRef.current;
    if (!root) return;
    const first = root.querySelector<HTMLElement>('button:not([disabled])');
    (first ?? root).focus({ preventScroll: true });

    const appRoot = document.getElementById('root');
    if (!appRoot) return;
    const hadInert = appRoot.hasAttribute('inert');
    const previousAriaHidden = appRoot.getAttribute('aria-hidden');
    appRoot.setAttribute('inert', '');
    appRoot.setAttribute('aria-hidden', 'true');
    return () => {
      if (!hadInert) appRoot.removeAttribute('inert');
      if (previousAriaHidden === null) appRoot.removeAttribute('aria-hidden');
      else appRoot.setAttribute('aria-hidden', previousAriaHidden);
    };
  }, [showBannerPicker]);

  // Runs after the effect above has removed `inert` from #root.
  useEffect(() => {
    if (showBannerPicker || !returnFocusAfterPicker.current) return;
    returnFocusAfterPicker.current = false;
    appearanceButtonRef.current?.focus({ preventScroll: true });
  }, [showBannerPicker]);

  // Stable identity so memoized children (ContentManager etc.) don't re-render
  // on unrelated parent updates (menu open/close, tab switches).
  const handleOpenFolder = useCallback(async (subdir: string) => {
    try {
      await invoke('cmd_open_instance_folder', { instanceName: selectedInstance, subfolder: subdir });
    } catch (e: any) { addToast(t('instance_detail.folder_error', { error: e.toString() }), 'error'); }
  }, [selectedInstance, t]);

  const openContentFolder = useCallback(() => {
    handleOpenFolder(tab === 'mods' ? 'mods' : tab);
  }, [handleOpenFolder, tab]);

  const selectTab = (tb: Tab) => {
    if (tb === tab) return;
    useBrowserGuardStore.getState().askLeave(() => setTab(tb));
  };

  const TABS: { id: Tab; label: string }[] = [
    { id: 'mods', label: t('instance_detail.tab_mods') },
    { id: 'resourcepacks', label: t('instance_detail.tab_resourcepacks') },
    { id: 'shaderpacks', label: t('instance_detail.tab_shaderpacks') },
    { id: 'worlds', label: t('instance_detail.tab_worlds') },
    { id: 'servers', label: t('instance_detail.tab_servers') },
    { id: 'screenshots', label: t('instance_detail.tab_screenshots') },
  ];

  useEffect(() => {
    if (!showMenu && !showBannerPicker) return;
    const handler = (e: MouseEvent) => {
      const target = e.target as Node;
      // Both refs are optional on purpose: the context menu unmounts while the
      // banner picker is open, so a null ref must mean "not inside", never
      // "give up and stay open".
      if (menuRef.current?.contains(target)) return;
      if (pickerRef.current?.contains(target)) return;
      if (showBannerPicker) {
        closeBannerPicker();
      } else {
        setShowMenu(false);
      }
    };
    document.addEventListener('mousedown', handler);
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.preventDefault();
        if (showBannerPicker) {
          closeBannerPicker();
        } else {
          setShowMenu(false);
          appearanceButtonRef.current?.focus();
        }
        return;
      }
      if (e.key !== 'Tab' || !showBannerPicker) return;
      const root = pickerRef.current;
      if (!root) return;
      const focusable = Array.from(
        root.querySelectorAll<HTMLElement>(
          'button:not([disabled]), a[href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])',
        ),
      ).filter((el) => el.getClientRects().length > 0);
      if (focusable.length === 0) {
        e.preventDefault();
        root.focus({ preventScroll: true });
        return;
      }
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      const active = document.activeElement;
      const inside = active instanceof Node && root.contains(active);
      if (e.shiftKey) {
        if (!inside || active === first || active === root) {
          e.preventDefault();
          last.focus({ preventScroll: true });
        }
      } else if (!inside || active === root || active === last) {
        e.preventDefault();
        first.focus({ preventScroll: true });
      }
    };
    document.addEventListener('keydown', onKey);
    return () => {
      document.removeEventListener('mousedown', handler);
      document.removeEventListener('keydown', onKey);
    };
  }, [showMenu, showBannerPicker, closeBannerPicker]);

  if (!instance) {
    return (
      <div style={{ flex: 1, display: 'flex', flexDirection: 'column', alignItems: 'center', justifyContent: 'center', color: 'var(--text-tertiary)', gap: 'var(--space-md)', padding: 'var(--space-3xl)' }}>
        <Package size={48} opacity={0.3} />
        <div style={{ fontSize: 'var(--font-size-lg)', fontWeight: 500 }}>{t('instance_detail.empty_heading')}</div>
        <div style={{ fontSize: 'var(--font-size-sm)' }}>{t('instance_detail.empty_desc')}</div>
      </div>
    );
  }

  const handlePickIcon = async () => {
    const selected = await openFileDialog({
      title: t('instance_detail.icon_file_title'),
      filters: [{ name: 'Images', extensions: ['png', 'jpg', 'jpeg', 'ico'] }],
      multiple: false,
    });
    if (!selected) return;
    try {
      const bytes = await invoke<number[]>('cmd_read_image_file', { path: selected });
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
      const bytes = await invoke<number[]>('cmd_read_image_file', { path: selected });
      const ext = selected.split('.').pop()?.toLowerCase() || 'png';
      const mime = ext === 'jpg' || ext === 'jpeg' ? 'image/jpeg' : 'image/png';
      let binary = '';
      for (let i = 0; i < bytes.length; i++) binary += String.fromCharCode(bytes[i]);
      const dataUrl = `data:${mime};base64,${btoa(binary)}`;
      await invoke('cmd_set_instance_banner', { instanceName: instance.name, bannerData: dataUrl });
      addToast(t('home.banner_updated'), 'success');
      useInstanceStore.getState().loadInstances();
    } catch (e: any) { addToast(e?.message || t('home.banner_error', { error: e?.message || 'unknown' }), 'error'); }
  };

  const setBannerGradient = async (gradientId: string) => {
    try {
      await invoke('cmd_set_instance_banner', { instanceName: instance.name, bannerData: `gradient:${gradientId}` });
      closeBannerPicker();
      useInstanceStore.getState().loadInstances();
    } catch (e: any) {
      addToast(e?.message || t('home.banner_error', { error: e?.message || 'unknown' }), 'error');
    }
  };

  const removeBanner = async () => {
    try {
      await invoke('cmd_set_instance_banner', { instanceName: instance.name, bannerData: '' });
      closeBannerPicker();
      useInstanceStore.getState().loadInstances();
    } catch (e: any) {
      addToast(e?.message || t('home.banner_remove_error', { error: e?.message || 'unknown' }), 'error');
    }
  };

  return (
    <div style={{ flex: 1, display: 'flex', flexDirection: 'column', overflow: 'hidden' }}>
      {/* Top bar */}
      <div style={{ padding: 'var(--space-md) var(--space-2xl)', borderBottom: '1px solid var(--surface-border)', display: 'flex', alignItems: 'center', gap: 'var(--space-lg)', flexShrink: 0 }}>
        <button
          type="button"
          ref={appearanceButtonRef}
          onClick={() => { setShowMenu(!showMenu); setShowBannerPicker(false); }}
          className="icon-editable"
          aria-label={t('instances.change_appearance')}
          aria-haspopup="menu"
          aria-expanded={showMenu}
          style={{ width: 40, height: 40, padding: 0, border: 'none', background: 'transparent', borderRadius: 'var(--radius-md)', overflow: 'hidden', cursor: 'pointer', flexShrink: 0, position: 'relative' }}
        >
          {/* The header square mirrors the banner (gradient or image) so it
              always matches what the home card shows; falls back to the
              custom icon, then to the letter tile. */}
          {isGradientBanner(instance.banner ?? '') ? (
            <div style={{ width: '100%', height: '100%', background: getGradientValue(instance.banner!), display: 'flex', alignItems: 'center', justifyContent: 'center', fontSize: 18, fontWeight: 700, color: 'white' }}>
              {instance.name.charAt(0).toUpperCase()}
            </div>
          ) : instance.banner ? (
            <img src={instance.banner} alt="" style={{ width: '100%', height: '100%', objectFit: 'cover' }} />
          ) : instance.icon ? (
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
        </button>

        {/* Icon context menu */}
        {showMenu && !showBannerPicker && (
          <div
            ref={menuRef}
            className="floating-menu floating-menu--bare"
            role="menu"
            aria-label={t('instances.change_appearance')}
            style={{ top: 52, left: 24 }}
            onClick={(e) => e.stopPropagation()}
          >
            <button
              type="button"
              role="menuitem"
              className="btn btn--ghost btn--sm"
              style={{ width: '100%', justifyContent: 'flex-start', gap: 'var(--space-sm)' }}
              onClick={() => setShowBannerPicker(true)}
            >
              <Image size={16} />
              {t('home.change_banner')}
            </button>
            <button
              type="button"
              role="menuitem"
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
        {showBannerPicker && createPortal(
          <div
            onClick={closeBannerPicker}
            style={{
              position: 'fixed',
              inset: 0,
              zIndex: 200,
              display: 'flex',
              alignItems: 'center',
              justifyContent: 'center',
            }}
          >
            <div
              ref={pickerRef}
              className="floating-menu floating-menu--picker"
              role="dialog"
              aria-modal="true"
              aria-label={t('home.banner_presets')}
              tabIndex={-1}
              style={{ width: 300, maxWidth: '90vw' }}
              onClick={(e) => e.stopPropagation()}
            >
              <div style={{ fontSize: 'var(--font-size-sm)', fontWeight: 600, marginBottom: 'var(--space-sm)' }}>
                {t('home.banner_presets')}
              </div>
              <div style={{ display: 'grid', gridTemplateColumns: 'repeat(4, 1fr)', gap: 'var(--space-xs)', marginBottom: 'var(--space-sm)' }}>
                {BANNER_PRESETS.map((p) => {
                  const selectedPreset =
                    isGradientBanner(instance.banner ?? '') &&
                    instance.banner === `gradient:${p.id}`;
                  return (
                    <Tooltip key={p.id} content={bannerLabel(p.id)}>
                      <button
                        type="button"
                        aria-label={bannerLabel(p.id)}
                        aria-pressed={selectedPreset}
                        onClick={() => setBannerGradient(p.id)}
                        style={{
                          width: '100%',
                          aspectRatio: '16/9',
                          padding: 0,
                          borderRadius: 'var(--radius-sm)',
                          background: p.gradient,
                          cursor: 'pointer',
                          border: selectedPreset ? '2px solid var(--primary)' : '2px solid transparent',
                          transition: 'border-color 0.15s',
                        }}
                      />
                    </Tooltip>
                  );
                })}
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
                  <Tooltip content={t('home.remove_banner')}>
                    <button
                      type="button"
                      className="btn btn--ghost btn--sm"
                      aria-label={t('home.remove_banner')}
                      style={{ justifyContent: 'center', color: 'var(--color-danger)' }}
                      onClick={removeBanner}
                    >
                      <X size={14} />
                    </button>
                  </Tooltip>
                )}
              </div>
            </div>
          </div>,
          document.body
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
          <Tooltip content={t('instances.ctx_settings')}>
            <Button
              size="sm"
              variant="ghost"
              aria-label={`${t('instances.ctx_settings')}: ${instance.name}`}
              onClick={() => setShowEditor(true)}
            >
              <Settings size={14} />
            </Button>
          </Tooltip>
          <Tooltip content={t('common.delete')}>
            <Button
              size="sm"
              variant="ghost"
              aria-label={`${t('common.delete')}: ${instance.name}`}
              onClick={() => setShowDeleteConfirm(true)}
              style={{ color: 'var(--color-danger)' }}
            >
              <Trash2 size={14} />
            </Button>
          </Tooltip>
        </div>
      </div>

      {/* Tabs: one frame slides between the buttons — the same motion the
          terminal filters and the modpack installers use. */}
      <div style={{ padding: '0 var(--space-2xl)', borderBottom: '1px solid var(--surface-border)', flexShrink: 0 }}>
        <SlidingTabs<Tab>
          tabs={TABS}
          active={tab}
          onChange={selectTab}
          ariaLabel={instance.name}
          className="instance-detail-tabs"
          controlsPanels
          idPrefix="instance-detail-tabs"
          style={{ marginBottom: 0, borderBottom: 'none' }}
        />
      </div>

      {/* Tab content: all tabpanel ids exist so every tab's aria-controls has a
          real target; only the active content is mounted, so switching still
          resets search/selection and keeps the previous sections cheap. */}
      <div style={{ flex: 1, display: 'flex', overflow: 'hidden' }}>
        {TABS.map((tb) => (
          <div
            key={tb.id}
            role="tabpanel"
            id={slidingPanelId('instance-detail-tabs', tb.id)}
            aria-labelledby={slidingTabId('instance-detail-tabs', tb.id)}
            hidden={tab !== tb.id}
            style={{ flex: 1, display: tab === tb.id ? 'flex' : 'none', overflow: 'hidden' }}
          >
            {tab === tb.id && (
              <>
                {(tb.id === 'mods' || tb.id === 'resourcepacks' || tb.id === 'shaderpacks') && (
                  <ContentManager
                    key={`${instance.name}-${tb.id}`}
                    instanceName={instance.name}
                    contentType={tb.id === 'mods' ? 'mod' : tb.id === 'resourcepacks' ? 'resourcepack' : 'shader'}
                    mcVersion={instance.mc_version}
                    loader={instance.loader}
                    onOpenFolder={openContentFolder}
                  />
                )}
                {tb.id === 'worlds' && (
                  <WorldsManager
                    key={instance.name}
                    instanceName={instance.name}
                    onOpenFolder={() => handleOpenFolder('saves')}
                  />
                )}
                {tb.id === 'servers' && (
                  <ServersManager key={instance.name} instanceName={instance.name} />
                )}
                {tb.id === 'screenshots' && (
                  <ScreenshotsGallery
                    key={instance.name}
                    instanceName={instance.name}
                    onOpenFolder={() => handleOpenFolder('screenshots')}
                  />
                )}
              </>
            )}
          </div>
        ))}
      </div>

      {/* Instance Editor Modal */}
      <InstanceEditor
        open={showEditor}
        instance={instance}
        onClose={() => setShowEditor(false)}
        onSaved={(newName) => {
          const store = useInstanceStore.getState();
          const wasSelected = store.selectedInstance === instance.name;
          store.loadInstances();
          // Keep the detail pane attached to the same instance after a rename.
          if (wasSelected) store.selectInstance(newName);
          setShowEditor(false);
        }}
      />

      {/* Delete Confirmation — shared confirm modal (lg: scrim + glass sheet) */}
      <ConfirmDialog
        open={showDeleteConfirm}
        title={t('instance_detail.confirm_delete_title')}
        description={t('instance_detail.confirm_delete_text', { name: instance.name })}
        onConfirm={() => {
          deleteInstance(instance.name);
          setShowDeleteConfirm(false);
        }}
        onCancel={() => setShowDeleteConfirm(false)}
        confirmLabel={t('common.delete')}
        variant="danger"
      />
    </div>
  );
}

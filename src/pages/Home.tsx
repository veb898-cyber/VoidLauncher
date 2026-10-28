import { useEffect, useMemo, useState, useRef } from 'react';
import { Play, Plus, Package, Clock, Image, Upload, X } from 'lucide-react';
import { useAuthStore } from '../stores/authStore';
import { useAccountsStore } from '../stores/accountsStore';
import { useInstanceStore } from '../stores/instanceStore';
import { useBrowserGuardStore } from '../stores/browserGuardStore';
import { SnakeSpinner } from '../components/ui/SnakeSpinner';
import { Tooltip } from '../components/ui/Tooltip';
import { useT, formatPlayTime, formatRelativeTime, type MessageKey } from '../lib/i18n';
import { createPortal } from 'react-dom';
import { addToast } from '../components/ui/Toast';
import { invoke } from '@tauri-apps/api/core';
import { BANNER_PRESETS, isGradientBanner, getGradientValue } from '../lib/bannerPresets';
import { Avatar } from '../lib/remoteImage';

interface HomeProps {
  onNavigate: (page: string) => void;
}

export function Home({ onNavigate }: HomeProps) {
  const t = useT();
  const { profile, isLoggedIn } = useAuthStore();
  const accounts = useAccountsStore((s) => s.accounts);
  const loadAccounts = useAccountsStore((s) => s.loadAccounts);
  const instances = useInstanceStore((s) => s.instances);
  const instancesLoading = useInstanceStore((s) => s.isLoading);
  const isLaunching = useInstanceStore((s) => s.isLaunching);
  const launchGame = useInstanceStore((s) => s.launchGame);
  const loadInstances = useInstanceStore((s) => s.loadInstances);
  const selectInstance = useInstanceStore((s) => s.selectInstance);

  const [menuInstance, setMenuInstance] = useState<string | null>(null);
  const [menuPos, setMenuPos] = useState<{ top: number; left: number } | null>(null);
  const [bannerPickerFor, setBannerPickerFor] = useState<string | null>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const pickerRef = useRef<HTMLDivElement>(null);
  const bannerLabel = (id: string) => t(`home.banner_${id.replace(/-/g, '_')}` as MessageKey);

  useEffect(() => {
    loadInstances();
    loadAccounts();
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Close icon menu on outside click. The icon itself is skipped: its own
  // click toggles the menu, and closing on mousedown would leave a null state
  // by the time the click arrives, so the icon would reopen it right away.
  useEffect(() => {
    if (!menuInstance) return;
    const handler = (e: MouseEvent) => {
      if ((e.target as HTMLElement).closest?.('.instance-card__icon')) return;
      if (menuRef.current && !menuRef.current.contains(e.target as Node)) {
        setMenuInstance(null);
      }
    };
    document.addEventListener('mousedown', handler);
    return () => document.removeEventListener('mousedown', handler);
  }, [menuInstance]);

  // Close banner picker on outside click
  useEffect(() => {
    if (!bannerPickerFor) return;
    const handler = (e: MouseEvent) => {
      if (pickerRef.current && !pickerRef.current.contains(e.target as Node)) {
        setBannerPickerFor(null);
      }
    };
    document.addEventListener('mousedown', handler);
    return () => document.removeEventListener('mousedown', handler);
  }, [bannerPickerFor]);

  const setBanner = async (instanceName: string, value: string) => {
    try {
      await invoke('cmd_set_instance_banner', { instanceName, bannerData: value });
      setBannerPickerFor(null);
      setMenuInstance(null);
      loadInstances();
    } catch (e: any) {
      addToast(e?.message || t('home.banner_error', { error: e?.message || 'unknown' }), 'error');
    }
  };

  const handlePickImage = async (instanceName: string, type: 'icon' | 'banner') => {
    try {
      const { open } = await import('@tauri-apps/plugin-dialog');
      const selected = await open({
        title: type === 'icon' ? t('home.dialog_choose_icon') : t('home.dialog_choose_banner'),
        filters: [{ name: t('home.dialog_images'), extensions: ['png', 'jpg', 'jpeg'] }],
        multiple: false,
      });
      if (!selected) return;
      const bytes = await invoke<number[]>('cmd_read_image_file', { path: selected });
      const ext = selected.split('.').pop()?.toLowerCase() || 'png';
      const mime = ext === 'jpg' || ext === 'jpeg' ? 'image/jpeg' : 'image/png';
      let binary = '';
      for (let i = 0; i < bytes.length; i++) binary += String.fromCharCode(bytes[i]);
      const dataUrl = `data:${mime};base64,${btoa(binary)}`;
      const cmd = type === 'icon' ? 'cmd_set_instance_icon' : 'cmd_set_instance_banner';
      await invoke(cmd, { instanceName, [`${type}Data`]: dataUrl });
      setBannerPickerFor(null);
      setMenuInstance(null);
      loadInstances();
    } catch (e: any) {
      const key = type === 'icon' ? 'home.icon_error' : 'home.banner_error';
      addToast(t(key, { error: e?.message || String(e) }), 'error');
    }
  };

  const lastInstance = useMemo(() => {
    if (instances.length === 0) return null;
    const sorted = [...instances].sort((a, b) => {
      const aTime = a.last_played || '';
      const bTime = b.last_played || '';
      return bTime.localeCompare(aTime);
    });
    return sorted[0];
  }, [instances]);

  const defaultAccount = useMemo(
    () => accounts.find((a) => a.default) ?? null,
    [accounts]
  );

  const activeName = defaultAccount?.name ?? profile?.name ?? null;
  const activeId = defaultAccount?.uuid ?? profile?.id ?? null;
  const hasAccount = !!defaultAccount || isLoggedIn;

  const handleQuickPlay = async () => {
    if (!hasAccount) {
      onNavigate('accounts');
      return;
    }
    if (lastInstance) {
      await launchGame(lastInstance.name);
    } else {
      onNavigate('instances');
    }
  };

  return (
    <div className="page" style={{ display: 'flex', flexDirection: 'column', gap: 'var(--space-xl)' }}>
      {/* Top bar: greeting + quick play */}
      <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-lg)' }}>
        {activeId ? (
          <Avatar uuid={activeId} name={activeName ?? ''} size={56} />
        ) : (
          <div style={{
            width: 56, height: 56,
            borderRadius: 'var(--radius-md)',
            display: 'flex', alignItems: 'center', justifyContent: 'center',
            fontSize: 24, fontWeight: 700, color: 'var(--text-tertiary)',
          }}>
            {(activeName ?? '?').charAt(0).toUpperCase()}
          </div>
        )}
        <div style={{ flex: 1 }}>
          <div style={{ fontSize: 'var(--font-size-xl)', fontWeight: 700 }}>
            {activeName
              ? t('home.greeting_logged_in', { name: activeName })
              : t('home.greeting_guest')}
          </div>
          <div style={{ color: 'var(--text-secondary)', fontSize: 'var(--font-size-sm)' }}>
            {!hasAccount
              ? t('home.subtitle_not_logged_in')
              : lastInstance
                ? t('home.subtitle_last_played', { name: lastInstance.name })
                : t('home.subtitle_no_instances')}
          </div>
        </div>
        <div style={{ display: 'flex', gap: 'var(--space-sm)' }}>
          <button
            className="btn btn--play"
            onClick={handleQuickPlay}
            disabled={isLaunching}
            id="quick-play-btn"
            style={{ padding: 'var(--space-md) var(--space-xl)' }}
          >
            {isLaunching ? (
              <><Play size={18} className="icon-breathe" /> {t('home.btn_launching')}</>
            ) : !hasAccount ? (
              <><Play size={18} /> {t('home.btn_login')}</>
            ) : lastInstance ? (
              <><Play size={18} fill="currentColor" /> {t('home.btn_play')}</>
            ) : (
              <><Plus size={18} /> {t('home.btn_create')}</>
            )}
          </button>
        </div>
      </div>

      {/* Instance grid */}
      <div style={{ flex: 1 }}>
        {instances.length > 0 && (
          <h2 style={{ fontSize: 'var(--font-size-lg)', fontWeight: 600, marginBottom: 'var(--space-md)' }}>
            {t('home.section_instances')}
          </h2>
        )}

        {instances.length === 0 && instancesLoading ? (
          <div style={{
            flex: 1,
            display: 'flex',
            alignItems: 'center',
            justifyContent: 'center',
            padding: 'var(--space-3xl)',
          }}>
            <SnakeSpinner size={28} />
          </div>
        ) : instances.length === 0 ? (
          <div style={{
            flex: 1,
            display: 'flex', flexDirection: 'column',
            alignItems: 'center', justifyContent: 'center',
            color: 'var(--text-tertiary)',
            gap: 'var(--space-md)',
            padding: 'var(--space-3xl)',
            borderRadius: 'var(--radius-lg)',
            border: '1px dashed var(--border)',
          }}>
            <Package size={48} opacity={0.3} />
            <div style={{ fontSize: 'var(--font-size-lg)', fontWeight: 500 }}>{t('home.empty_title')}</div>
            <div style={{ fontSize: 'var(--font-size-sm)', textAlign: 'center', maxWidth: 300 }}>
              {t('home.empty_desc')}
            </div>
            <button
              className="btn btn--primary"
              onClick={() => onNavigate('instances')}
            >
              <Plus size={16} /> {t('home.empty_cta')}
            </button>
          </div>
        ) : (
          <div className="instance-grid">
            {instances.map((inst) => (
              <div
                key={inst.name}
                className="instance-card"
                onClick={() => useBrowserGuardStore.getState().askLeave(() => {
                  selectInstance(inst.name);
                  onNavigate('instances');
                })}
                role="button"
                tabIndex={0}
                onKeyDown={(e) => {
                  if (e.key === 'Enter' || e.key === ' ') {
                    e.preventDefault();
                    useBrowserGuardStore.getState().askLeave(() => {
                      selectInstance(inst.name);
                      onNavigate('instances');
                    });
                  }
                }}
              >
                <div className="instance-card__banner" style={isGradientBanner(inst.banner) ? {
                  background: getGradientValue(inst.banner),
                  opacity: 1,
                } : undefined}>
                  <div className="instance-card__banner-overlay"></div>
                  {inst.banner && !isGradientBanner(inst.banner) ? (
                    <img
                      className="instance-card__banner-img"
                      src={inst.banner}
                      alt={inst.name}
                    />
                  ) : inst.icon && !inst.banner ? (
                    <img
                      className="instance-card__banner-img"
                      src={inst.icon}
                      alt={inst.name}
                    />
                  ) : (
                    <div className="instance-card__banner-fallback">
                      <Package size={32} />
                    </div>
                  )}
                  {inst.last_played && (
                    <div className="instance-card__last-played">
                      <Clock size={12} />
                      <span>{formatRelativeTime(inst.last_played)}</span>
                    </div>
                  )}
                </div>
                <div className="instance-card__body instance-card__body--horizontal">
                  <div
                    className="instance-card__icon icon-editable"
                    style={{ position: 'relative', cursor: 'pointer' }}
                    role="button"
                    tabIndex={0}
                    aria-label={t('home.change_icon')}
                    aria-haspopup="menu"
                    onClick={(e) => {
                      e.stopPropagation();
                      if (menuInstance !== inst.name) {
                        // Capture the card's viewport rect: the menu is portaled
                        // to <body> (a child of the glass card would sample only
                        // the card's backdrop root and miss the card/text), so it
                        // needs fixed coordinates kept identical to the old
                        // absolute (top 148, left 16) placement inside the card.
                        const card = (e.currentTarget as HTMLElement).closest('.instance-card') as HTMLElement | null;
                        const r = card?.getBoundingClientRect();
                        setMenuPos(r ? { top: r.top + 148, left: r.left + 16 } : null);
                      }
                      setMenuInstance(menuInstance === inst.name ? null : inst.name);
                      setBannerPickerFor(null);
                    }}
                    onKeyDown={(e) => {
                      if (e.key === 'Enter' || e.key === ' ') {
                        e.preventDefault();
                        (e.currentTarget as HTMLElement).click();
                      }
                    }}
                  >
                    {inst.icon ? (
                      <img src={inst.icon} alt={inst.name} />
                    ) : (
                      <div className="instance-card__icon-fallback">
                        <Package size={20} />
                      </div>
                    )}
                  </div>
                  <div className="instance-card__info">
                    <div className="instance-card__name">{inst.name}</div>
                    <div className="instance-card__meta">
                      <span className="instance-card__version">{inst.mc_version}</span>
                      {inst.loader && inst.loader !== 'Vanilla' && (
                        <span className={`instance-card__tag instance-card__tag--${inst.loader.toLowerCase()}`}>
                          {inst.loader}
                          {inst.loader_version ? ` ${inst.loader_version}` : ''}
                        </span>
                      )}
                    </div>
                  </div>
                </div>
                <div className="instance-card__actions">
                  <span className="instance-card__playtime">
                    <Clock size={12} />
                    {inst.play_time_seconds && inst.play_time_seconds > 0
                      ? formatPlayTime(inst.play_time_seconds)
                      : t('home.instance_never_played')}
                  </span>
                  <button
                    className="instance-card__play"
                    onClick={(e) => {
                      e.stopPropagation();
                      launchGame(inst.name);
                    }}
                    disabled={isLaunching}
                  >
                    <Play size={14} fill="currentColor" />
                    <span>{t('home.instance_play_btn')}</span>
                  </button>
                </div>

                {/* Icon context menu — shared floating-menu surface (lg: glass).
                    Portaled to <body>: inside the glass card the menu would be
                    trapped in the card's backdrop root and blur only the
                    wallpaper, not the card, its text and everything beneath it. */}
                {menuInstance === inst.name && !bannerPickerFor && createPortal(
                  <div
                    ref={menuRef}
                    className="floating-menu floating-menu--bare"
                    style={{
                      position: 'fixed',
                      top: menuPos?.top ?? 148,
                      left: menuPos?.left ?? 16,
                      zIndex: 100,
                    }}
                    onClick={(e) => e.stopPropagation()}
                  >
                    <button
                      className="btn btn--ghost btn--sm"
                      style={{ width: '100%', justifyContent: 'flex-start', gap: 'var(--space-sm)' }}
                      onClick={() => setBannerPickerFor(inst.name)}
                    >
                      <Image size={16} />
                      {t('home.change_banner')}
                    </button>
                    <button
                      className="btn btn--ghost btn--sm"
                      style={{ width: '100%', justifyContent: 'flex-start', gap: 'var(--space-sm)' }}
                      onClick={() => handlePickImage(inst.name, 'icon')}
                    >
                      <Upload size={16} />
                      {t('home.change_icon')}
                    </button>
                  </div>,
                  document.body
                )}

                {/* Banner preset picker */}
                {bannerPickerFor === inst.name && createPortal(
                  <div
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
                      style={{
                        width: 300,
                        maxWidth: '90vw',
                      }}
                      onClick={(e) => e.stopPropagation()}
                    >
                      <div style={{ fontSize: 'var(--font-size-sm)', fontWeight: 600, marginBottom: 'var(--space-sm)' }}>
                        {t('home.banner_presets')}
                      </div>
                      <div style={{ display: 'grid', gridTemplateColumns: 'repeat(4, 1fr)', gap: 'var(--space-xs)', marginBottom: 'var(--space-sm)' }}>
                        {BANNER_PRESETS.map((p) => (
                          <Tooltip key={p.id} content={bannerLabel(p.id)}>
                            <div
                              role="button"
                              tabIndex={0}
                              aria-label={bannerLabel(p.id)}
                              aria-pressed={inst.banner === `gradient:${p.id}`}
                              onClick={() => setBanner(inst.name, `gradient:${p.id}`)}
                              onKeyDown={(e) => {
                                if (e.key === 'Enter' || e.key === ' ') {
                                  e.preventDefault();
                                  setBanner(inst.name, `gradient:${p.id}`);
                                }
                              }}
                              style={{
                                width: '100%',
                                aspectRatio: '16/9',
                                borderRadius: 'var(--radius-sm)',
                                background: p.gradient,
                                cursor: 'pointer',
                                border: inst.banner === `gradient:${p.id}` ? '2px solid var(--primary)' : '2px solid transparent',
                                transition: 'border-color 0.15s',
                              }}
                            />
                          </Tooltip>
                        ))}
                      </div>
                      <div style={{ display: 'flex', gap: 'var(--space-xs)' }}>
                        <button
                          className="btn btn--ghost btn--sm"
                          style={{ flex: 1, justifyContent: 'center', gap: 'var(--space-xs)' }}
                          onClick={() => handlePickImage(inst.name, 'banner')}
                        >
                          <Upload size={14} />
                          {t('home.upload_image')}
                        </button>
                        {inst.banner && (
                          <Tooltip content={t('home.remove_banner')}>
                            <button
                              className="btn btn--ghost btn--sm"
                              style={{ justifyContent: 'center', color: 'var(--color-danger)' }}
                              aria-label={t('home.remove_banner')}
                              onClick={() => setBanner(inst.name, '')}
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
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

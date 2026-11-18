import { useCallback, useEffect, useRef, useState, type ReactNode } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Coffee, FlaskConical, Globe, Info, Palette, ScanSearch, SlidersHorizontal } from 'lucide-react';
import { useSettingsStore, type AppConfig } from '../stores/settingsStore';
import { useBrowserGuardStore } from '../stores/browserGuardStore';
import { useLanguageStore } from '../stores/languageStore';
import { useFontStore } from '../stores/fontStore';
import { Button } from '../components/ui/Button';
import { ConfirmDialog } from '../components/ui/ConfirmDialog';
import { RefreshIcon } from '../components/ui/RefreshIcon';
import { SlidingTabs, slidingPanelId, slidingTabId, type SlidingTabDef } from '../components/ui/SlidingTabs';
import { addToast } from '../components/ui/Toast';
import { useT } from '../lib/i18n';
import { JavaSettings } from './settings/JavaSettings';
import { AppearanceSettings } from './settings/AppearanceSettings';
import { GeneralSettings } from './settings/GeneralSettings';
import { NetworkSettings } from './settings/NetworkSettings';
import { DiagnosticsSettings } from './settings/DiagnosticsSettings';
import { AboutSettings } from './settings/AboutSettings';
import { ExperimentalSettings } from './settings/ExperimentalSettings';
import {
  SETTINGS_CATEGORY_IDS,
  SETTINGS_CATEGORY_LABEL_KEYS,
  initialSettingsCategory,
  type SettingsCategory,
} from './settings/settingsLayout';
import type { UpdateConfig } from './settings/types';

const CATEGORY_ICONS: Record<SettingsCategory, ReactNode> = {
  general: <SlidersHorizontal size={15} aria-hidden />,
  appearance: <Palette size={15} aria-hidden />,
  java: <Coffee size={15} aria-hidden />,
  network: <Globe size={15} aria-hidden />,
  experimental: <FlaskConical size={15} aria-hidden />,
  diagnostics: <ScanSearch size={15} aria-hidden />,
  about: <Info size={15} aria-hidden />,
};

export function Settings() {
  const t = useT();
  const config = useSettingsStore((s) => s.config);
  const loadConfig = useSettingsStore((s) => s.loadConfig);
  const loadError = useSettingsStore((s) => s.loadError);
  const configLoading = useSettingsStore((s) => s.isLoading);
  const saveConfig = useSettingsStore((s) => s.saveConfig);
  const language = useLanguageStore((s) => s.language);
  const font = useFontStore((s) => s.font);
  const [localConfig, setLocalConfig] = useState<AppConfig | null>(config);
  const [saveSuccess, setSaveSuccess] = useState(false);
  const [pendingRoomsDisable, setPendingRoomsDisable] = useState(false);
  const [saving, setSaving] = useState(false);
  const [category, setCategory] = useState<SettingsCategory>(initialSettingsCategory);
  const pageRef = useRef<HTMLDivElement>(null);
  // Network toggles apply immediately (so downloads react before Save), which
  // means leaving the page with unsaved edits must restore the persisted
  // policy; otherwise the runtime keeps a proxy the user just discarded.
  const persistedConfigRef = useRef(config);
  persistedConfigRef.current = config;

  useEffect(() => {
    loadConfig();
  }, [loadConfig]);

  useEffect(() => () => {
    const persisted = persistedConfigRef.current;
    if (!persisted) return;
    void invoke('cmd_apply_proxy_policy', {
      proxyEnabled: persisted.proxy_enabled,
      useSystemProxy: persisted.use_system_proxy,
      proxyAddr: persisted.proxy_addr,
      proxyPort: persisted.proxy_port,
      proxyOnly: persisted.proxy_only,
    }).catch(() => {
      // Closing Settings must never surface an IPC error; the next launch
      // re-applies the persisted policy anyway.
    });
  }, []);

  useEffect(() => {
    setLocalConfig(config);
  }, [config]);

  const updateConfig = useCallback<UpdateConfig>((key, value) => {
    setLocalConfig((prev) => (prev ? { ...prev, [key]: value } : prev));
  }, []);

  const dirty = !!localConfig && !!config && JSON.stringify(localConfig) !== JSON.stringify(config);

  // While there are unsaved changes, page navigation asks for confirmation
  // (shared leave-guard, see browserGuardStore). Switching between settings
  // categories does not go through the guard: the draft stays in this
  // component, so nothing is lost.
  useEffect(() => {
    useBrowserGuardStore.getState().setUnsaved(dirty);
    return () => { useBrowserGuardStore.getState().setUnsaved(false); };
  }, [dirty]);

  const performSave = useCallback(async (cfg: AppConfig) => {
    setSaving(true);
    try {
      await saveConfig(cfg);
      setSaveSuccess(true);
      addToast(t('settings.save_success'), 'success');
      setTimeout(() => setSaveSuccess(false), 2000);
    } catch (e) {
      addToast(t('settings.save_error', { error: String(e) }), 'error');
    } finally {
      setSaving(false);
    }
  }, [saveConfig, t]);

  const handleSave = useCallback(async () => {
    if (!localConfig) return;
    // Turning the experimental Rooms feature off during an active session
    // must warn first: the backend will end the room (guest loses access,
    // bridge stops, room code is cleared) as part of this save.
    const disablingRooms = !!config?.rooms_enabled && !localConfig.rooms_enabled;
    if (disablingRooms) {
      try {
        const active = await invoke<boolean>('cmd_room_session_active');
        if (active) {
          setPendingRoomsDisable(true);
          return;
        }
      } catch {
        // A status probe failure must not block disabling the feature; the
        // backend teardown is still safe when there is no room.
      }
    }
    await performSave(localConfig);
  }, [localConfig, config, performSave]);

  const handleCategoryChange = useCallback((next: SettingsCategory) => {
    if (next === category) return;
    setCategory(next);
    // The page itself scrolls inside .main-content; each category starts at
    // its own top instead of inheriting the previous page's scroll offset.
    const scroller = pageRef.current?.closest('.main-content');
    if (scroller instanceof HTMLElement) scroller.scrollTop = 0;
  }, [category]);

  if (!localConfig) {
    return (
      <div className="page">
        <div className="page__header">
          <h1 className="page__title">{t('settings.title')}</h1>
        </div>
        {loadError && !configLoading ? (
          <div className="glass-card" style={{ padding: 'var(--space-lg)', display: 'flex', flexDirection: 'column', gap: 'var(--space-sm)', alignItems: 'flex-start' }}>
            <div style={{ fontWeight: 600 }}>{t('settings.load_error_title')}</div>
            <div style={{ fontSize: 'var(--font-size-sm)', color: 'var(--text-secondary)' }}>
              {t('settings.load_error_desc', { error: loadError })}
            </div>
            <Button variant="secondary" onClick={() => loadConfig()}>
              <RefreshIcon spinning={configLoading} />
              {t('common.retry')}
            </Button>
          </div>
        ) : (
          <div className="skeleton" style={{ height: 200 }} />
        )}
      </div>
    );
  }

  const tabs: SlidingTabDef<SettingsCategory>[] = SETTINGS_CATEGORY_IDS.map((id) => ({
    id,
    label: <>{CATEGORY_ICONS[id]} {t(SETTINGS_CATEGORY_LABEL_KEYS[id])}</>,
  }));

  // One page per category, keyed by the same ids as the tab row. The Record
  // type makes a missing or unknown category a compile error, so the active
  // tab can never point at a page that does not exist.
  const pages: Record<SettingsCategory, ReactNode> = {
    general: <GeneralSettings config={localConfig} updateConfig={updateConfig} />,
    appearance: <AppearanceSettings />,
    java: <JavaSettings config={localConfig} updateConfig={updateConfig} />,
    network: <NetworkSettings config={localConfig} updateConfig={updateConfig} />,
    experimental: <ExperimentalSettings config={localConfig} updateConfig={updateConfig} />,
    diagnostics: <DiagnosticsSettings />,
    about: <AboutSettings />,
  };

  const pageClass = (id: SettingsCategory) =>
    `settings-page${category === id ? ' settings-page--active' : ''}`;

  return (
    <div className="page" ref={pageRef}>
      <div className="page__header">
        <h1 className="page__title">{t('settings.title')}</h1>
      </div>

      {/* Labels change width with the language and the interface font;
          remount so the sliding indicator is measured against the new text. */}
      <SlidingTabs<SettingsCategory>
        key={`${language}-${font}`}
        tabs={tabs}
        active={category}
        onChange={handleCategoryChange}
        ariaLabel={t('settings.nav_label')}
        className="settings-nav"
        controlsPanels
        idPrefix="settings-tabs"
      />

      <div className="settings-pages">
        {SETTINGS_CATEGORY_IDS.map((id) => (
          <div
            key={id}
            className={pageClass(id)}
            hidden={category !== id}
            role="tabpanel"
            id={slidingPanelId('settings-tabs', id)}
            aria-labelledby={slidingTabId('settings-tabs', id)}
          >
            {pages[id]}
          </div>
        ))}
      </div>

      {dirty && (
        <div className="settings-save-fab">
          <button className="btn btn--primary" onClick={handleSave} id="save-settings-btn" disabled={saving}>
            {saveSuccess ? `✓ ${t('settings.save_btn')}` : t('settings.save_btn')}
          </button>
        </div>
      )}

      <ConfirmDialog
        open={pendingRoomsDisable}
        title={t('settings.rooms_disable_title')}
        description={t('settings.rooms_disable_desc')}
        confirmLabel={t('settings.rooms_disable_confirm')}
        variant="primary"
        busy={saving}
        onCancel={() => setPendingRoomsDisable(false)}
        onConfirm={() => {
          setPendingRoomsDisable(false);
          if (localConfig) void performSave(localConfig);
        }}
      />
    </div>
  );
}

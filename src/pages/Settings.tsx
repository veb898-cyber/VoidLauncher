import { useCallback, useEffect, useRef, useState } from 'react';
import { Coffee, Globe, Info, Palette, RefreshCw, ScanSearch, SlidersHorizontal } from 'lucide-react';
import { useSettingsStore, type AppConfig } from '../stores/settingsStore';
import { useBrowserGuardStore } from '../stores/browserGuardStore';
import { useLanguageStore } from '../stores/languageStore';
import { useFontStore } from '../stores/fontStore';
import { Button } from '../components/ui/Button';
import { SlidingTabs, type SlidingTabDef } from '../components/ui/SlidingTabs';
import { addToast } from '../components/ui/Toast';
import { useT } from '../lib/i18n';
import { JavaSettings } from './settings/JavaSettings';
import { AppearanceSettings } from './settings/AppearanceSettings';
import { LauncherSettings } from './settings/LauncherSettings';
import { NetworkSettings } from './settings/NetworkSettings';
import { DiagnosticsSettings } from './settings/DiagnosticsSettings';
import { AboutSettings } from './settings/AboutSettings';
import type { UpdateConfig } from './settings/types';

const CATEGORY_IDS = [
  'java',
  'appearance',
  'launcher',
  'network',
  'diagnostics',
  'about',
] as const;

type SettingsCategory = (typeof CATEGORY_IDS)[number];

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
  const [category, setCategory] = useState<SettingsCategory>('java');
  const pageRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    loadConfig();
  }, [loadConfig]);

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

  const handleSave = useCallback(async () => {
    if (!localConfig) return;
    try {
      await saveConfig(localConfig);
      setSaveSuccess(true);
      addToast(t('settings.save_success'), 'success');
      setTimeout(() => setSaveSuccess(false), 2000);
    } catch (e) {
      addToast(t('settings.save_error', { error: String(e) }), 'error');
    }
  }, [localConfig, saveConfig, t]);

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
              <RefreshCw size={14} style={{ marginRight: 6 }} />
              {t('common.retry')}
            </Button>
          </div>
        ) : (
          <div className="skeleton" style={{ height: 200 }} />
        )}
      </div>
    );
  }

  const tabs: SlidingTabDef<SettingsCategory>[] = [
    {
      id: 'java',
      label: <><Coffee size={15} aria-hidden /> {t('settings.section_java')}</>,
    },
    {
      id: 'appearance',
      label: <><Palette size={15} aria-hidden /> {t('settings.section_appearance')}</>,
    },
    {
      id: 'launcher',
      label: <><SlidersHorizontal size={15} aria-hidden /> {t('settings.section_launcher')}</>,
    },
    {
      id: 'network',
      label: <><Globe size={15} aria-hidden /> {t('settings.section_network')}</>,
    },
    {
      id: 'diagnostics',
      label: <><ScanSearch size={15} aria-hidden /> {t('settings.section_diagnostics')}</>,
    },
    {
      id: 'about',
      label: <><Info size={15} aria-hidden /> {t('settings.section_about')}</>,
    },
  ];

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
      />

      <div className="settings-pages">
        <div className={pageClass('java')} hidden={category !== 'java'}>
          <JavaSettings config={localConfig} updateConfig={updateConfig} />
        </div>
        <div className={pageClass('appearance')} hidden={category !== 'appearance'}>
          <AppearanceSettings />
        </div>
        <div className={pageClass('launcher')} hidden={category !== 'launcher'}>
          <LauncherSettings config={localConfig} updateConfig={updateConfig} />
        </div>
        <div className={pageClass('network')} hidden={category !== 'network'}>
          <NetworkSettings config={localConfig} updateConfig={updateConfig} />
        </div>
        <div className={pageClass('diagnostics')} hidden={category !== 'diagnostics'}>
          <DiagnosticsSettings />
        </div>
        <div className={pageClass('about')} hidden={category !== 'about'}>
          <AboutSettings />
        </div>
      </div>

      {dirty && (
        <div className="settings-save-fab">
          <button className="btn btn--primary" onClick={handleSave} id="save-settings-btn">
            {saveSuccess ? `✓ ${t('settings.save_btn')}` : t('settings.save_btn')}
          </button>
        </div>
      )}
    </div>
  );
}

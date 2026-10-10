import { useEffect, useState, useCallback } from 'react';
import { useSettingsStore } from '../stores/settingsStore';
import { useAuthStore } from '../stores/authStore';
import { useAccountsStore } from '../stores/accountsStore';
import { useJavaDownloadStore } from '../stores/javaDownloadStore';
import { useBrowserGuardStore } from '../stores/browserGuardStore';
import { open as openFileDialog } from '@tauri-apps/plugin-dialog';
import { openUrl } from '@tauri-apps/plugin-opener';
import { invoke } from '@tauri-apps/api/core';
import { Button } from '../components/ui/Button';
import { LoadingSpinner } from '../components/ui/LoadingSpinner';
import { addToast } from '../components/ui/Toast';
import { CustomSelect } from '../components/ui/CustomSelect';
import { getRecommendedMemoryMb, snapToMemoryStep } from '../lib/memory';
import { useT, type Language } from '../lib/i18n';
import { useLanguageStore } from '../stores/languageStore';
import { useThemeStore, Theme } from '../stores/themeStore';
import { useWallpaperStore } from '../stores/wallpaperStore';
import type { WallpaperId } from '../lib/wallpapers';
import { useFontStore, Font, ConsoleFont } from '../stores/fontStore';
import { Avatar, OfflineAvatar } from '../lib/remoteImage';
import { Tooltip } from '../components/ui/Tooltip';
import { CheckCircle2, AlertTriangle, XCircle, Circle } from 'lucide-react';
import { useInstanceStore } from '../stores/instanceStore';
import {
  CHECK_LABELS, ISSUE_KEYS, VERIFY_CATEGORY_LABELS,
  type DiagReport,
} from '../lib/diagnostics';
import { friendlyErrorMessage } from '../lib/errors';
import {
  APP_VERSION,
  useLatestVersion,
  getVersionComparison,
} from '../hooks/useLatestVersion';

interface AvailableJavaVersion {
  major_version: number;
  label: string;
}

interface ManagedJavaRuntime {
  major_version: number;
  path: string;
  version: string;
  vendor: string;
  is_64bit: boolean;
}

export function Settings() {
  const t = useT();
  const language = useLanguageStore((s) => s.language);
  const theme = useThemeStore((s) => s.theme);
  const setTheme = useThemeStore((s) => s.setTheme);
  const liquidGlass = useThemeStore((s) => s.liquidGlass);
  const setLiquidGlass = useThemeStore((s) => s.setLiquidGlass);
  const realisticGlass = useThemeStore((s) => s.realisticGlass);
  const setRealisticGlass = useThemeStore((s) => s.setRealisticGlass);
  const wallpaper = useWallpaperStore((s) => s.wallpaper);
  const setWallpaper = useWallpaperStore((s) => s.setWallpaper);

  // Name of the normal theme, so the Liquid Glass note can say which theme
  // will come back instead of speaking about "the theme".
  const themeLabel: Record<Theme, string> = {
    standard: t('settings.theme_standard'),
    blueprint: t('settings.theme_blueprint'),
    ember: t('settings.theme_ember'),
    dark: t('settings.theme_dark'),
  };
  const font = useFontStore((s) => s.font);
  const consoleFont = useFontStore((s) => s.consoleFont);
  const config = useSettingsStore((s) => s.config);
  const javaInstallations = useSettingsStore((s) => s.javaInstallations);
  const loadConfig = useSettingsStore((s) => s.loadConfig);
  const saveConfig = useSettingsStore((s) => s.saveConfig);
  const detectJava = useSettingsStore((s) => s.detectJava);
  const detectSystemRam = useSettingsStore((s) => s.detectSystemRam);
  const { profile, isLoggedIn, logout } = useAuthStore();
  const accounts = useAccountsStore((s) => s.accounts);
  const loadAccounts = useAccountsStore((s) => s.loadAccounts);
  const [localConfig, setLocalConfig] = useState(config);
  const [saveSuccess, setSaveSuccess] = useState(false);
  const [clearingCache, setClearingCache] = useState(false);
  const [availableJava, setAvailableJava] = useState<AvailableJavaVersion[]>([]);
  const [managedJava, setManagedJava] = useState<ManagedJavaRuntime[]>([]);
  const [checkingJava, setCheckingJava] = useState(false);
  const [javaError, setJavaError] = useState(false);
  const javaDownload = useJavaDownloadStore((s) => s.active);
  const selectedInstance = useInstanceStore((s) => s.selectedInstance);
  const [diagReport, setDiagReport] = useState<DiagReport | null>(null);
  const [diagRunning, setDiagRunning] = useState(false);
  const [diagError, setDiagError] = useState<string | null>(null);

  const runDiagnostics = useCallback(async () => {
    setDiagRunning(true);
    setDiagError(null);
    try {
      setDiagReport(await invoke<DiagReport>('cmd_run_diagnostics', {
        instanceName: selectedInstance,
      }));
    } catch (e) {
      setDiagError(friendlyErrorMessage(e, t));
    } finally {
      setDiagRunning(false);
    }
  }, [selectedInstance, t]);

  const diagStatusIcon = (status: string) =>
    status === 'ok' ? <CheckCircle2 size={15} color="var(--color-success)" />
    : status === 'warn' ? <AlertTriangle size={15} color="var(--color-warning)" />
    : status === 'error' ? <XCircle size={15} color="var(--color-danger)" />
    : <Circle size={15} color="var(--text-tertiary)" />;

  useEffect(() => {
    loadConfig();
    detectJava();
    loadAccounts();
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    setLocalConfig(config);
  }, [config]);

  const loadJavaVersions = useCallback(async () => {
    setCheckingJava(true);
    setJavaError(false);
    try {
      const [available, managed] = await Promise.all([
        invoke<AvailableJavaVersion[]>('cmd_list_available_java'),
        invoke<ManagedJavaRuntime[]>('cmd_list_managed_java'),
      ]);
      setAvailableJava(available);
      setManagedJava(managed);
      if (available.length === 0) {
        setJavaError(true);
      }
    } catch {
      setJavaError(true);
    } finally {
      setCheckingJava(false);
    }
  }, []);

  useEffect(() => {
    loadJavaVersions();
  }, [loadJavaVersions]);

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

  const updateConfig = (key: string, value: any) => {
    if (!localConfig) return;
    const updated = { ...localConfig, [key]: value };
    setLocalConfig(updated);
  };

  const dirty = !!localConfig && !!config && JSON.stringify(localConfig) !== JSON.stringify(config);

  // While there are unsaved changes, page navigation asks for confirmation
  // (shared leave-guard, see browserGuardStore).
  useEffect(() => {
    useBrowserGuardStore.getState().setUnsaved(dirty);
    return () => { useBrowserGuardStore.getState().setUnsaved(false); };
  }, [dirty]);

  if (!localConfig) {
    return (
      <div className="page">
        <div className="page__header">
          <h1 className="page__title">{t('settings.title')}</h1>
        </div>
        <div className="skeleton" style={{ height: 200 }} />
      </div>
    );
  }

  return (
    <div className="page">
      <div className="page__header">
        <h1 className="page__title">{t('settings.title')}</h1>
      </div>

      {/* Account Section */}
      <section className="settings-section">
        <h2 className="settings-section__title">{t('settings.section_account')}</h2>
        <AccountCard
          defaultAccountName={accounts.find((a) => a.default)?.name ?? null}
          defaultAccountUuid={accounts.find((a) => a.default)?.uuid ?? null}
          defaultAccountType={accounts.find((a) => a.default)?.account_type ?? null}
          microsoftName={isLoggedIn && profile ? profile.name : null}
          microsoftUuid={isLoggedIn && profile ? profile.id : null}
          onSignOut={logout}
        />
      </section>

      {/* Java Section */}
      <section className="settings-section">
        <h2 className="settings-section__title">{t('settings.section_java')}</h2>

        <div className="settings-row">
          <div className="settings-row__info">
            <div className="settings-row__title">{t('settings.java_path_title')}</div>
            <div className="settings-row__desc">
              {t('settings.java_path_desc')}
            </div>
          </div>
          <div className="settings-row__control" style={{ display: 'flex', gap: 'var(--space-sm)', alignItems: 'center' }}>
            <input
              className="input"
              type="text"
              value={localConfig.java_path || ''}
              placeholder={t('settings.java_path_placeholder')}
              onChange={(e) => updateConfig('java_path', e.target.value || null)}
              style={{ width: 250, fontSize: 'var(--font-size-xs)' }}
            />
            <Button size="sm" variant="secondary" onClick={async () => {
              const selected = await openFileDialog({
                title: t('settings.java_dialog_title'),
                filters: [{ name: 'Java', extensions: ['exe'] }],
                multiple: false,
              });
              if (selected) updateConfig('java_path', selected);
            }}>{t('settings.java_browse')}</Button>
          </div>
        </div>

        <div className="settings-row">
          <div className="settings-row__info">
            <div className="settings-row__title">{t('settings.memory_title')}</div>
            <div className="settings-row__desc">
              {t('settings.memory_desc')}
            </div>
          </div>
          <div className="settings-row__control" style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-sm)' }}>
            <input
              className="input"
              type="number"
              min="2048"
              max="32768"
              step="512"
              value={localConfig.default_memory_mb}
              onChange={(e) => updateConfig('default_memory_mb', Math.max(2048, parseInt(e.target.value) || 2048))}
              style={{ width: 100, textAlign: 'center' }}
              id="min-memory-input"
            />
            <span style={{ color: 'var(--text-secondary)', fontSize: 'var(--font-size-sm)' }}>{t('settings.memory_unit')}</span>
            <Tooltip content={t('settings.memory_reset_tooltip')}>
              <button
                className="btn btn--secondary btn--sm"
                onClick={async () => {
                  const totalMb = await detectSystemRam();
                  // Tiered: 4 / 6 / 8 GB depending on total RAM, snapped to 512 MB step
                  const recommended = snapToMemoryStep(getRecommendedMemoryMb(totalMb));
                  updateConfig('default_memory_mb', recommended);
                }}
                id="reset-memory-btn"
              >
                {t('settings.memory_reset')}
              </button>
            </Tooltip>
          </div>
        </div>

        <div className="settings-row">
          <div className="settings-row__info">
            <div className="settings-row__title">{t('settings.gc_title')}</div>
            <div className="settings-row__desc">
              {t('settings.gc_desc')}
            </div>
          </div>
          <div className="settings-row__control" style={{ width: 320 }}>
            <CustomSelect
              value={localConfig.default_gc_preset}
              options={[
                { value: 'standard', label: t('instance_editor.gc_standard'), description: t('instance_editor.gc_standard_short') },
                { value: 'g1gc', label: t('instance_editor.gc_g1gc'), description: t('instance_editor.gc_g1gc_short') },
                { value: 'zgc', label: t('instance_editor.gc_zgc'), description: t('instance_editor.gc_zgc_req') },
              ]}
              onChange={(v) => updateConfig('default_gc_preset', v)}
            />
          </div>
        </div>

        {/* Detected Java Installations */}
        <div style={{ marginTop: 'var(--space-lg)' }}>
          <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', marginBottom: 'var(--space-md)' }}>
            <span style={{ fontWeight: 500 }}>{t('settings.java_detected_heading')}</span>
            <button className="btn btn--secondary btn--sm" onClick={detectJava}>
              {t('settings.java_refresh')}
            </button>
          </div>
          {javaInstallations.length === 0 ? (
            <div className="glass-card" style={{ padding: 'var(--space-lg)', textAlign: 'center', color: 'var(--text-secondary)' }}>
              {t('settings.java_none_detected')}
            </div>
          ) : (
            <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--space-xs)' }}>
              {javaInstallations.map((java, i) => (
                <div key={i} className="glass-card" style={{
                  padding: 'var(--space-md) var(--space-lg)',
                  display: 'flex',
                  alignItems: 'center',
                  justifyContent: 'space-between',
                }}>
                  <div>
                    <div style={{ fontWeight: 500, fontSize: 'var(--font-size-md)' }}>
                      Java {java.major_version}
                      <span style={{
                        marginLeft: 'var(--space-sm)',
                        fontSize: 'var(--font-size-xs)',
                        color: 'var(--text-tertiary)',
                      }}>
                        {java.version}
                      </span>
                    </div>
                    <div style={{ fontSize: 'var(--font-size-sm)', color: 'var(--text-secondary)' }}>
                      {java.vendor} &bull; {java.is_64bit ? t('settings.java_64bit') : t('settings.java_32bit')}
                    </div>
                  </div>
                  <span style={{
                    fontSize: 'var(--font-size-xs)',
                    color: 'var(--text-tertiary)',
                    maxWidth: 200,
                    overflow: 'hidden',
                    textOverflow: 'ellipsis',
                    whiteSpace: 'nowrap',
                  }}>
                    {java.path}
                  </span>
                </div>
              ))}
            </div>
          )}
        </div>
      </section>

      {/* Managed Java Runtimes */}
      <section className="settings-section">
        <h2 className="settings-section__title">{t('settings.java_download_heading')}</h2>
        <p className="settings-section__desc" style={{ marginTop: 'var(--space-xs)', marginBottom: 'var(--space-md)', color: 'var(--text-secondary)', fontSize: 'var(--font-size-sm)' }}>
          {t('settings.java_download_desc')}
        </p>

        {/* Available for download */}
        {checkingJava ? (
          <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-sm)', padding: 'var(--space-md)' }}>
            <LoadingSpinner size={16} />
            <span style={{ color: 'var(--text-secondary)', fontSize: 'var(--font-size-sm)' }}>{t('settings.java_checking')}</span>
          </div>
        ) : javaError && availableJava.length === 0 ? (
          <div className="glass-card" style={{ padding: 'var(--space-lg)', textAlign: 'center' }}>
            <div style={{ color: 'var(--text-secondary)', marginBottom: 'var(--space-sm)', fontSize: 'var(--font-size-sm)' }}>
              {t('settings.java_fetch_error')}
            </div>
            <Button size="sm" variant="secondary" onClick={loadJavaVersions}>
              {t('settings.java_retry')}
            </Button>
          </div>
        ) : (
          <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--space-xs)', marginBottom: 'var(--space-lg)' }}>
            {availableJava.map((jv) => {
              const isDownloading = javaDownload?.majorVersion === jv.major_version;
              const isInstalled = managedJava.some((m) => m.major_version === jv.major_version);
              return (
                <div key={jv.major_version} className="glass-card" style={{
                  padding: 'var(--space-md) var(--space-lg)',
                  display: 'flex',
                  alignItems: 'center',
                  justifyContent: 'space-between',
                }}>
                   <div>
                    <div style={{ fontWeight: 500, fontSize: 'var(--font-size-md)' }}>{jv.label}</div>
                    {isDownloading && javaDownload && (
                      <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-secondary)', marginTop: '2px' }}>
                        {javaDownload.message}
                      </div>
                    )}
                  </div>
                  <Button
                    size="sm"
                    variant={isInstalled ? 'secondary' : 'primary'}
                    disabled={isDownloading}
                    onClick={async () => {
                      if (isInstalled || isDownloading) return;
                      useJavaDownloadStore.getState().startDownload(jv.major_version);
                      try {
                        await invoke('cmd_download_java', { majorVersion: jv.major_version });
                        addToast(t('settings.java_install_success', { version: jv.major_version.toString() }), 'success');
                        const managed = await invoke<ManagedJavaRuntime[]>('cmd_list_managed_java');
                        setManagedJava(managed);
                      } catch (e) {
                        addToast(t('settings.java_install_error', { error: String(e) }), 'error');
                      } finally {
                        useJavaDownloadStore.getState().clear();
                      }
                    }}
                  >
                    {isDownloading ? <><LoadingSpinner size={14} /> {javaDownload ? `${Math.round(javaDownload.percent)}%` : ''}</> : isInstalled ? `✓ ${t('settings.java_downloaded')}` : t('settings.java_download_btn')}
                  </Button>
                </div>
              );
            })}
          </div>
        )}

        {/* Already downloaded */}
        <div style={{ fontWeight: 500, marginBottom: 'var(--space-md)' }}>{t('settings.java_downloaded')}</div>
        {managedJava.length === 0 ? (
          <div className="glass-card" style={{ padding: 'var(--space-lg)', textAlign: 'center', color: 'var(--text-secondary)' }}>
            {t('settings.java_no_managed')}
          </div>
        ) : (
          <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--space-xs)' }}>
            {managedJava.map((m) => (
              <div key={m.major_version} className="glass-card" style={{
                padding: 'var(--space-md) var(--space-lg)',
                display: 'flex',
                alignItems: 'center',
                justifyContent: 'space-between',
              }}>
                <div>
                  <div style={{ fontWeight: 500, fontSize: 'var(--font-size-md)' }}>
                    Java {m.major_version}
                    <span style={{ marginLeft: 'var(--space-sm)', fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)' }}>
                      {m.version}
                    </span>
                  </div>
                  <div style={{ fontSize: 'var(--font-size-sm)', color: 'var(--text-secondary)' }}>
                    {m.vendor} &bull; {m.is_64bit ? t('settings.java_64bit') : t('settings.java_32bit')}
                  </div>
                </div>
                <Button size="sm" variant="secondary" onClick={async () => {
                  try {
                    await invoke('cmd_remove_managed_java', { majorVersion: m.major_version });
                    addToast(t('settings.java_remove_success', { version: m.major_version.toString() }), 'success');
                    const managed = await invoke<ManagedJavaRuntime[]>('cmd_list_managed_java');
                    setManagedJava(managed);
                  } catch (e) {
                    addToast(t('settings.java_remove_error', { error: String(e) }), 'error');
                  }
                }}>
                  {t('settings.java_remove_btn')}
                </Button>
              </div>
            ))}
          </div>
        )}
      </section>

      {/* Appearance Section */}
      <section className="settings-section">
        <h2 className="settings-section__title">{t('settings.section_appearance')}</h2>
        <div className="settings-row">
          <div className="settings-row__info">
            <div className="settings-row__title">{t('settings.language_title')}</div>
            <div className="settings-row__desc">
              {t('settings.language_desc')}
            </div>
          </div>
          <div className="settings-row__control" style={{ width: 220 }}>
            <CustomSelect
              value={language}
              options={[
                { value: 'en', label: 'English' },
                { value: 'ru', label: 'Русский' },
              ]}
              onChange={(v) => useLanguageStore.getState().setLanguage(v as Language)}
            />
          </div>
        </div>
        <div className="settings-row">
          <div className="settings-row__info">
            <div className="settings-row__title">{t('settings.font_title')}</div>
            <div className="settings-row__desc">
              {t('settings.font_desc')}
            </div>
          </div>
          <div className="settings-row__control" style={{ width: 220 }}>
            <CustomSelect
              value={font}
              options={[
                { value: 'default', label: t('settings.font_default') },
                { value: 'monocraft', label: t('settings.font_monocraft') },
              ]}
              onChange={(v) => useFontStore.getState().setFont(v as Font)}
            />
          </div>
        </div>
        <div className="settings-row">
          <div className="settings-row__info">
            <div className="settings-row__title">{t('settings.console_font_title')}</div>
            <div className="settings-row__desc">
              {t('settings.console_font_desc')}
            </div>
          </div>
          <div className="settings-row__control" style={{ width: 220 }}>
            <CustomSelect
              value={consoleFont}
              options={[
                {
                  value: 'auto',
                  label: t('settings.console_font_auto'),
                  tooltip: t('settings.console_font_auto_hint'),
                },
                {
                  value: 'default',
                  label: t('settings.console_font_default'),
                  tooltip: t('settings.console_font_default_hint'),
                },
                {
                  value: 'monocraft',
                  label: t('settings.console_font_monocraft'),
                },
              ]}
              onChange={(v) => useFontStore.getState().setConsoleFont(v as ConsoleFont)}
            />
          </div>
        </div>
        <div className="settings-row">
          <div className="settings-row__info">
            <div className="settings-row__title">{t('settings.theme_title')}</div>
            <div className="settings-row__desc">
              {t('settings.theme_desc')}
            </div>
            {liquidGlass && (
              <div className="settings-row__note settings-row__note--locked">
                {t('settings.theme_locked')}
              </div>
            )}
          </div>
          <div className="settings-row__control" style={{ width: 220 }}>
            <CustomSelect
              value={theme}
              disabled={liquidGlass}
              options={[
                { value: 'standard', label: t('settings.theme_standard') },
                { value: 'blueprint', label: t('settings.theme_blueprint') },
                { value: 'ember', label: t('settings.theme_ember') },
                { value: 'dark', label: t('settings.theme_dark') },
              ].map((o) => ({ ...o, disabled: liquidGlass }))}
              onChange={(v) => setTheme(v as Theme)}
            />
          </div>
        </div>

        {/* Liquid Glass is a separate experimental rendering mode, not a
            colour preset of the theme above: while it is on, the normal
            themes stay visible but locked, and the selected one is kept so
            it can be restored on the way back. */}
        <div className="experimental-block">
          <div className="experimental-block__head">
            <span className="experimental-block__title">{t('settings.exp_section_title')}</span>
            <span className="experimental-badge">{t('common.experimental')}</span>
          </div>
          <div className="experimental-block__desc">{t('settings.exp_section_desc')}</div>
          <div className="settings-row">
            <div className="settings-row__info">
              <div className="settings-row__title">
                {t('settings.lg_title')}
                <span className="experimental-badge experimental-badge--inline">
                  {t('common.experimental')}
                </span>
              </div>
              <div className="settings-row__desc">{t('settings.lg_desc')}</div>
              {liquidGlass && (
                <div className="settings-row__note settings-row__note--active">
                  {t('settings.lg_active', { theme: themeLabel[theme] })}
                </div>
              )}
            </div>
            <div className="settings-row__control">
              <label className="toggle">
                <input
                  type="checkbox"
                  checked={liquidGlass}
                  onChange={(e) => setLiquidGlass(e.target.checked)}
                />
                <span className="toggle__slider" />
              </label>
            </div>
          </div>
          {/* The backdrop only exists under Liquid Glass, so it is disabled
              rather than hidden in the other appearances: the choice is kept and
              comes back when Liquid Glass is switched on again. */}
          <div className="settings-row">
            <div className="settings-row__info">
              <div className="settings-row__title">
                {t('settings.wallpaper_title')}
                <span className="experimental-badge experimental-badge--inline">
                  {t('common.experimental')}
                </span>
              </div>
              <div className="settings-row__desc">{t('settings.wallpaper_desc')}</div>
              {!liquidGlass && (
                <div className="settings-row__note settings-row__note--locked">
                  {t('settings.wallpaper_locked')}
                </div>
              )}
            </div>
            <div className="settings-row__control" style={{ width: 220 }}>
              <CustomSelect<WallpaperId>
                value={wallpaper}
                disabled={!liquidGlass}
                options={[
                  { value: 'none', label: t('settings.wallpaper_none') },
                  { value: 'waves', label: t('settings.wallpaper_waves') },
                  { value: 'ribbons', label: t('settings.wallpaper_ribbons') },
                  { value: 'mesh', label: t('settings.wallpaper_mesh') },
                  { value: 'nebula', label: t('settings.wallpaper_nebula') },
                ]}
                onChange={setWallpaper}
              />
            </div>
          </div>
          {/* Realistic liquid glass (owner pass, iOS 27 reference): a much
              clearer fill so the backdrop and content read through the panels.
              Off by default; locked like the backdrop choice while Liquid Glass
              is off, and the selection is kept for the next time it is on. */}
          <div className="settings-row">
            <div className="settings-row__info">
              <div className="settings-row__title">
                {t('settings.lg_realistic_title')}
                <span className="experimental-badge experimental-badge--inline">
                  {t('common.experimental')}
                </span>
              </div>
              <div className="settings-row__desc">{t('settings.lg_realistic_desc')}</div>
              {!liquidGlass && (
                <div className="settings-row__note settings-row__note--locked">
                  {t('settings.lg_realistic_locked')}
                </div>
              )}
            </div>
            <div className="settings-row__control">
              <label className="toggle">
                <input
                  type="checkbox"
                  checked={realisticGlass}
                  disabled={!liquidGlass}
                  onChange={(e) => setRealisticGlass(e.target.checked)}
                />
                <span className="toggle__slider" />
              </label>
            </div>
          </div>
        </div>
      </section>

      {/* Launcher Section */}
      <section className="settings-section">
        <h2 className="settings-section__title">{t('settings.section_launcher')}</h2>

        <div className="settings-row">
          <div className="settings-row__info">
            <div className="settings-row__title">{t('settings.close_on_launch_title')}</div>
            <div className="settings-row__desc">
              {t('settings.close_on_launch_desc')}
            </div>
          </div>
          <div className="settings-row__control">
            <label className="toggle">
              <input
                type="checkbox"
                checked={localConfig.close_on_launch}
                onChange={(e) => updateConfig('close_on_launch', e.target.checked)}
              />
              <span className="toggle__slider" />
            </label>
          </div>
        </div>

        <div className="settings-row">
          <div className="settings-row__info">
            <div className="settings-row__title">{t('settings.data_dir_title')}</div>
            <div className="settings-row__desc">
              {localConfig.data_dir}
            </div>
          </div>
          <div className="settings-row__control">
            <Button size="sm" variant="secondary" onClick={async () => {
              await invoke('cmd_open_folder', { path: localConfig.data_dir });
            }}>{t('settings.data_dir_open')}</Button>
          </div>
        </div>

        <div className="settings-row">
          <div className="settings-row__info">
            <div className="settings-row__title">{t('settings.game_logs_title')}</div>
            <div className="settings-row__desc">
              {t('settings.game_logs_desc')}
            </div>
          </div>
          <div className="settings-row__control">
            <Button size="sm" variant="secondary" onClick={async () => {
              await invoke('cmd_open_folder', { path: `${localConfig.data_dir}/logs/game` });
            }}>{t('settings.game_logs_open')}</Button>
          </div>
        </div>

        <div className="settings-row">
          <div className="settings-row__info">
            <div className="settings-row__title">{t('settings.clear_cache_title')}</div>
            <div className="settings-row__desc">
              {t('settings.clear_cache_desc')}
            </div>
          </div>
          <div className="settings-row__control">
            <Button
              size="sm"
              variant="secondary"
              disabled={clearingCache}
              onClick={async () => {
                setClearingCache(true);
                try {
                  const freed = await invoke<number>('cmd_clear_cache');
                  const mb = Math.round(freed / 1024 / 1024);
                  addToast(t('settings.clear_cache_success', { mb: mb.toString() }), 'success');
                } catch (e) {
                  addToast(t('settings.clear_cache_error', { error: String(e) }), 'error');
                } finally {
                  setClearingCache(false);
                }
              }}
            >
              {clearingCache ? <LoadingSpinner size={14} /> : t('settings.clear_cache_btn')}
            </Button>
          </div>
        </div>
      </section>

      {/* Network Section */}
      <section className="settings-section">
        <h2 className="settings-section__title">{t('settings.section_network')}</h2>

        <div className="settings-row">
          <div className="settings-row__info">
            <div className="settings-row__title">{t('settings.proxy_enabled_title')}</div>
            <div className="settings-row__desc">
              {t('settings.proxy_enabled_desc')}
            </div>
          </div>
          <div className="settings-row__control">
            <label className="toggle">
              <input
                type="checkbox"
                checked={localConfig.proxy_enabled}
                onChange={(e) => updateConfig('proxy_enabled', e.target.checked)}
              />
              <span className="toggle__slider" />
            </label>
          </div>
        </div>

        <div className="settings-row settings-row--stacked" style={{ opacity: localConfig.proxy_enabled ? 1 : 0.5 }}>
          <div className="settings-row__info">
            <div className="settings-row__title">{t('settings.proxy_server_title')}</div>
          </div>
          <div className="settings-row__control">
            <div className="field-group field-group--grow">
              <label className="field-group__label" htmlFor="proxy-addr">
                {t('settings.proxy_addr_title')}
              </label>
              <input
                id="proxy-addr"
                className="input"
                type="text"
                value={localConfig.proxy_addr}
                disabled={!localConfig.proxy_enabled}
                onChange={(e) => updateConfig('proxy_addr', e.target.value)}
              />
              <span className="field-group__hint">{t('settings.proxy_addr_desc')}</span>
            </div>
            <div className="field-group">
              <label className="field-group__label" htmlFor="proxy-port">
                {t('settings.proxy_port_title')}
              </label>
              <input
                id="proxy-port"
                className="input"
                type="number"
                min={1}
                max={65535}
                style={{ width: 120 }}
                value={localConfig.proxy_port || ''}
                disabled={!localConfig.proxy_enabled}
                onChange={(e) => updateConfig('proxy_port', Math.max(1, Math.min(65535, Number(e.target.value) || 0)))}
              />
              <span className="field-group__hint">{t('settings.proxy_port_desc')}</span>
            </div>
          </div>
        </div>
      </section>

      {/* Diagnostics Section */}
      <section className="settings-section">
        <h2 className="settings-section__title">{t('settings.section_diagnostics')}</h2>
        <p
          className="settings-section__desc"
          style={{ marginTop: 'var(--space-xs)', marginBottom: 'var(--space-md)', color: 'var(--text-secondary)', fontSize: 'var(--font-size-sm)' }}
        >
          {t('diag.desc')}
        </p>

        <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-md)', marginBottom: 'var(--space-md)', flexWrap: 'wrap' }}>
          <Button onClick={runDiagnostics} disabled={diagRunning}>
            {diagRunning ? t('diag.running') : t('diag.run')}
          </Button>
          {selectedInstance && (
            <span style={{ fontSize: 'var(--font-size-sm)', color: 'var(--text-secondary)' }}>
              {t('diag.instance_scope', { name: selectedInstance })}
            </span>
          )}
          {!selectedInstance && (
            <span style={{ fontSize: 'var(--font-size-sm)', color: 'var(--text-tertiary)' }}>
              {t('diag.no_instance')}
            </span>
          )}
        </div>
        {diagError && (
          <div style={{ color: 'var(--color-danger)', fontSize: 'var(--font-size-sm)', marginBottom: 'var(--space-md)' }}>
            {diagError}
          </div>
        )}

        {diagReport && (
          <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--space-lg)' }}>
            {/* System inventory */}
            <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fill, minmax(250px, 1fr))', gap: 'var(--space-sm) var(--space-xl)' }}>
              {diagReport.checks.map((c) => (
                <div key={c.id} style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-sm)', fontSize: 'var(--font-size-sm)', minWidth: 0 }}>
                  {diagStatusIcon(c.status)}
                  <span style={{ color: 'var(--text-secondary)', whiteSpace: 'nowrap' }}>
                    {CHECK_LABELS[c.id] ? t(CHECK_LABELS[c.id]) : c.id}
                  </span>
                  <span
                    style={{
                      marginLeft: 'auto', fontWeight: 500, minWidth: 0,
                      overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap',
                      maxWidth: '55%', textAlign: 'right',
                    }}
                    title={c.value}
                  >
                    {c.value}
                  </span>
                </div>
              ))}
            </div>

            {/* Instance files breakdown (from the offline verify scan) */}
            {diagReport.verify && diagReport.verify.categories.length > 0 && (
              <div>
                <div style={{ fontSize: 'var(--font-size-sm)', fontWeight: 600, marginBottom: 'var(--space-sm)' }}>
                  {t('diag.files_breakdown')}
                </div>
                <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fill, minmax(250px, 1fr))', gap: 'var(--space-sm) var(--space-xl)' }}>
                  {diagReport.verify.categories.map((cat) => {
                    const ok = cat.issue_count === 0;
                    return (
                      <div key={cat.id} style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-sm)', fontSize: 'var(--font-size-sm)', minWidth: 0 }}>
                        {ok ? <CheckCircle2 size={15} color="var(--color-success)" /> : <XCircle size={15} color="var(--color-danger)" />}
                        <span style={{ color: 'var(--text-secondary)', whiteSpace: 'nowrap' }}>
                          {VERIFY_CATEGORY_LABELS[cat.id] ? t(VERIFY_CATEGORY_LABELS[cat.id]) : cat.id}
                        </span>
                        <span style={{ marginLeft: 'auto', fontWeight: 500, color: ok ? undefined : 'var(--color-danger)' }}>
                          {ok ? t('verify.checked_count', { n: cat.checked }) : String(cat.issue_count)}
                        </span>
                      </div>
                    );
                  })}
                </div>
              </div>
            )}

            {/* Detected problems */}
            <div>
              <div style={{ fontSize: 'var(--font-size-sm)', fontWeight: 600, marginBottom: 'var(--space-sm)' }}>
                {t('diag.issues_heading')}
              </div>
              {diagReport.issues.length === 0 ? (
                <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-sm)', fontSize: 'var(--font-size-sm)', color: 'var(--color-success)' }}>
                  <CheckCircle2 size={15} />
                  <span>{t('diag.no_issues')}</span>
                </div>
              ) : (
                <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--space-sm)' }}>
                  {diagReport.issues.map((issue, i) => {
                    const keys = ISSUE_KEYS[issue.id];
                    return (
                      <div key={`${issue.id}-${i}`} style={{ display: 'flex', gap: 'var(--space-sm)', alignItems: 'flex-start' }}>
                        {issue.severity === 'error'
                          ? <XCircle size={15} color="var(--color-danger)" style={{ flexShrink: 0, marginTop: 2 }} />
                          : <AlertTriangle size={15} color="var(--color-warning)" style={{ flexShrink: 0, marginTop: 2 }} />}
                        <div style={{ minWidth: 0 }}>
                          <div style={{ fontSize: 'var(--font-size-sm)', fontWeight: 500 }}>
                            {keys ? t(keys.message, issue.params) : issue.id}
                          </div>
                          {keys && (
                            <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)' }}>
                              {t(keys.fix)}
                            </div>
                          )}
                        </div>
                      </div>
                    );
                  })}
                </div>
              )}
            </div>
          </div>
        )}
      </section>

      {/* About Section */}
      <section className="settings-section">
        <h2 className="settings-section__title">{t('settings.section_about')}</h2>
        <div className="glass-card" style={{ padding: 'var(--space-xl)' }}>
          <div style={{ fontWeight: 700, fontSize: 'var(--font-size-xl)', marginBottom: 'var(--space-sm)' }}>
            <span style={{
              background: 'var(--gradient-primary)',
              WebkitBackgroundClip: 'text',
              WebkitTextFillColor: 'transparent',
            }}>
              {t('settings.about_name')}
            </span>
            {' '}
            <span style={{ color: 'var(--text-tertiary)', fontSize: 'var(--font-size-md)', fontWeight: 400 }}>{t('settings.about_version', { version: APP_VERSION })}</span>
          </div>
          <p style={{ color: 'var(--text-secondary)', fontSize: 'var(--font-size-sm)' }}>
            {t('settings.about_desc')}
            <br />
            {t('settings.about_disclaimer')}
          </p>
        </div>
      </section>

      <LatestVersionSection />

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

interface AccountCardProps {
  /** The user's default account, if any (any type). */
  defaultAccountName: string | null;
  defaultAccountUuid: string | null;
  defaultAccountType: 'Microsoft' | 'Offline' | 'ElyBy' | null;
  /** Fallback Microsoft profile from the auth store, when no default account
   *  exists yet but the user is currently signed in to Microsoft. */
  microsoftName: string | null;
  microsoftUuid: string | null;
  onSignOut: () => void;
}

/**
 * Resolves which account to show in the Account section.
 *
 *   1. Default account from the accounts list (any type: Microsoft, Ely.by, Offline).
 *   2. Fallback: legacy Microsoft profile from authStore.
 *   3. No account at all.
 *
 * For Microsoft accounts, shows a "Sign Out" button (clears the cached
 * Microsoft tokens). For Ely.by / Offline, no destructive action is
 * exposed here — the user manages those in the Accounts tab.
 */
function AccountCard({
  defaultAccountName,
  defaultAccountUuid,
  defaultAccountType,
  microsoftName,
  microsoftUuid,
  onSignOut,
}: AccountCardProps) {
  const t = useT();

  const typeLabelKey = defaultAccountType
    ? defaultAccountType === 'Microsoft'
      ? 'settings.account_type_microsoft'
      : defaultAccountType === 'ElyBy'
        ? 'settings.account_type_elyby'
        : 'settings.account_type_offline'
    : null;

  const isMicrosoft = defaultAccountType === 'Microsoft';
  const hasAccount = !!defaultAccountName || !!microsoftName;
  const activeName = defaultAccountName ?? microsoftName ?? '';
  const activeUuid = defaultAccountUuid ?? microsoftUuid ?? null;
  // Свежий UUID текущего профиля Microsoft надёжнее, чем из сохранённого
  // списка аккаунтов: id активной сессии точно рабочий (тот же, что в сайдбаре),
  // тогда как сохранённое значение может устареть или отличаться форматом.
  const avatarUuid = microsoftUuid ?? defaultAccountUuid ?? null;
  const typeLabel = typeLabelKey ? t(typeLabelKey) : '';

  return (
    <div className="glass-card" style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-xl)', padding: 'var(--space-xl)' }}>
      {hasAccount ? (
        <>
          {defaultAccountType === 'Offline' ? (
            <OfflineAvatar size={48} />
          ) : (
            <Avatar uuid={avatarUuid ?? ''} name={activeName ?? ''} size={48} />
          )}
          <div style={{ flex: 1 }}>
            <div style={{ fontWeight: 600, fontSize: 'var(--font-size-lg)' }}>{activeName}</div>
            <div style={{ color: 'var(--text-secondary)', fontSize: 'var(--font-size-sm)' }}>
              {t('settings.account_subtitle_default', { type: typeLabel })}
              {activeUuid ? ` \u2022 ${activeUuid.substring(0, 8)}\u2026` : ''}
            </div>
          </div>
          {isMicrosoft && (
            <button className="btn btn--danger" onClick={onSignOut}>
              {t('settings.sign_out')}
            </button>
          )}
        </>
      ) : (
        <>
          <div style={{
            width: 48, height: 48,
            borderRadius: 'var(--radius-md)',
            display: 'flex', alignItems: 'center', justifyContent: 'center',
            fontSize: 20, fontWeight: 700, color: 'var(--text-tertiary)',
          }}>
            ?
          </div>
          <div style={{ flex: 1 }}>
            <div style={{ fontWeight: 600 }}>{t('settings.not_signed_in')}</div>
            <div style={{ color: 'var(--text-secondary)', fontSize: 'var(--font-size-sm)' }}>
              {t('settings.sign_in_subtitle')}
            </div>
          </div>
        </>
      )}
    </div>
  );
}

function LatestVersionSection() {
  const t = useT();
  const { latest, loading, error, refresh, checked } = useLatestVersion();
  const cmp = getVersionComparison(APP_VERSION, latest);
  const showUpdateLabel = checked && latest && cmp.updateAvailable;
  const showUpToDate = checked && latest && !cmp.updateAvailable && cmp.status === 0;
  const showDev = checked && latest && cmp.status === 1;

  const statusColor = showUpdateLabel
    ? 'var(--warning)'
    : showUpToDate
    ? 'var(--success)'
    : 'var(--text-secondary)';

  return (
    <section className="settings-section">
      <h2 className="settings-section__title">{t('settings.latest_version_label')}</h2>
      <div
        className="glass-card"
        style={{
          padding: 'var(--space-lg) var(--space-xl)',
          display: 'flex',
          alignItems: 'center',
          gap: 'var(--space-lg)',
          flexWrap: 'wrap',
        }}
      >
        <div style={{ flex: 1, minWidth: 220 }}>
          <div
            style={{
              display: 'flex',
              alignItems: 'baseline',
              gap: 'var(--space-sm)',
              flexWrap: 'wrap',
            }}
          >
            <span style={{ color: 'var(--text-tertiary)', fontSize: 'var(--font-size-sm)' }}>
              {t('settings.latest_version_current')}:
            </span>
            <span
              style={{
                fontWeight: 600,
                fontFamily: 'var(--font-ui)',
                color: 'var(--text-primary)',
              }}
            >
              v{APP_VERSION}
            </span>
            <span style={{ color: 'var(--text-tertiary)' }}>→</span>
            <span style={{ color: 'var(--text-tertiary)', fontSize: 'var(--font-size-sm)' }}>
              {t('settings.latest_version_label')}:
            </span>
            <span
              style={{
                fontWeight: 600,
                fontFamily: 'var(--font-ui)',
                color: statusColor,
              }}
            >
              {loading
                ? t('settings.latest_version_checking')
                : !checked
                ? '—'
                : latest
                ? `v${latest}`
                : '—'}
            </span>
          </div>
          <div
            style={{
              fontSize: 'var(--font-size-xs)',
              color: statusColor,
              marginTop: 'var(--space-xs)',
              minHeight: '1.2em',
            }}
          >
            {showUpdateLabel && t('settings.latest_version_available', { version: latest! })}
            {showUpToDate && t('settings.latest_version_up_to_date')}
            {showDev && `v${APP_VERSION} (dev)`}
            {checked && error && t('settings.latest_version_failed')}
          </div>
        </div>
        <div style={{ display: 'flex', gap: 'var(--space-sm)' }}>
          {showUpdateLabel && (
            <Button
              size="sm"
              variant="primary"
              onClick={async () => {
                try {
                  await openUrl(
                    'https://github.com/veb898-cyber/VoidLauncher/releases/latest',
                  );
                } catch {
                  /* best-effort */
                }
              }}
            >
              {t('settings.latest_version_check_btn')}
            </Button>
          )}
          <Button size="sm" variant="secondary" onClick={refresh} disabled={loading}>
            {loading ? <LoadingSpinner size={14} /> : t('settings.latest_version_check_btn')}
          </Button>
        </div>
      </div>
    </section>
  );
}

import { useEffect, useState, useCallback } from 'react';
import { useSettingsStore } from '../stores/settingsStore';
import { useAuthStore } from '../stores/authStore';
import { useAccountsStore } from '../stores/accountsStore';
import { open as openFileDialog } from '@tauri-apps/plugin-dialog';
import { openUrl } from '@tauri-apps/plugin-opener';
import { invoke } from '@tauri-apps/api/core';
import { Button } from '../components/ui/Button';
import { LoadingSpinner } from '../components/ui/LoadingSpinner';
import { addToast } from '../components/ui/Toast';
import { CustomSelect } from '../components/ui/CustomSelect';
import { getRecommendedMemoryMb, snapToMemoryStep } from '../lib/memory';
import { useT, Language } from '../lib/i18n';
import { useLanguageStore } from '../stores/languageStore';
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
  const [downloadingJava, setDownloadingJava] = useState<number | null>(null);
  const [javaError, setJavaError] = useState(false);

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
      setTimeout(() => setSaveSuccess(false), 2000);
    } catch {
    }
  }, [localConfig, saveConfig]);

  const updateConfig = (key: string, value: any) => {
    if (!localConfig) return;
    const updated = { ...localConfig, [key]: value };
    setLocalConfig(updated);
  };

  if (!localConfig) {
    return (
      <div className="page animate-fade-in">
        <div className="page__header">
          <h1 className="page__title">{t('settings.title')}</h1>
        </div>
        <div className="skeleton" style={{ height: 200 }} />
      </div>
    );
  }

  return (
    <div className="page animate-fade-in">
      <div className="page__header" style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
        <div>
          <h1 className="page__title">{t('settings.title')}</h1>
          <p className="page__subtitle">{t('settings.subtitle')}</p>
        </div>
        <button className="btn btn--primary" onClick={handleSave} id="save-settings-btn" disabled={!localConfig}>
          {saveSuccess ? `✓ ${t('settings.save_btn')}` : t('settings.save_btn')}
        </button>
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
              placeholder={t('settings.java_placeholder')}
              value={localConfig.java_path || ''}
              onChange={(e) => updateConfig('java_path', e.target.value || null)}
              style={{ width: 250, fontSize: 'var(--font-size-xs)' }}
            />
            <Button size="sm" variant="ghost" onClick={async () => {
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
              title={t('settings.memory_title')}
            />
            <span style={{ color: 'var(--text-secondary)', fontSize: 'var(--font-size-sm)' }}>{t('settings.memory_unit')}</span>
            <button
              className="btn btn--ghost btn--sm"
              onClick={async () => {
                const totalMb = await detectSystemRam();
                // Tiered: 4 / 6 / 8 GB depending on total RAM, snapped to 512 MB step
                const recommended = snapToMemoryStep(getRecommendedMemoryMb(totalMb));
                updateConfig('default_memory_mb', recommended);
              }}
              id="reset-memory-btn"
              title={t('settings.memory_reset_tooltip')}
            >
              {t('settings.memory_reset')}
            </button>
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
                { value: 'standard', label: 'Standard', description: t('instance_editor.gc_standard_desc') },
                { value: 'g1gc', label: t('instance_editor.gc_g1gc'), description: 'Java 8+' },
                { value: 'zgc', label: t('instance_editor.gc_zgc'), description: 'Java 17+, \u2265 6 GB' },
              ]}
              onChange={(v) => updateConfig('default_gc_preset', v)}
            />
          </div>
        </div>

        {/* Detected Java Installations */}
        <div style={{ marginTop: 'var(--space-lg)' }}>
          <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', marginBottom: 'var(--space-md)' }}>
            <span style={{ fontWeight: 500 }}>{t('settings.java_detected_heading')}</span>
            <button className="btn btn--ghost btn--sm" onClick={detectJava}>
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
              const isDownloading = downloadingJava === jv.major_version;
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
                  </div>
                  <Button
                    size="sm"
                    variant={isInstalled ? 'secondary' : 'primary'}
                    disabled={isDownloading}
                    onClick={async () => {
                      if (isInstalled) return;
                      setDownloadingJava(jv.major_version);
                      try {
                        await invoke('cmd_download_java', { majorVersion: jv.major_version });
                        addToast(t('settings.java_install_success', { version: jv.major_version.toString() }), 'success');
                        const managed = await invoke<ManagedJavaRuntime[]>('cmd_list_managed_java');
                        setManagedJava(managed);
                      } catch (e) {
                        addToast(t('settings.java_install_error', { error: String(e) }), 'error');
                      } finally {
                        setDownloadingJava(null);
                      }
                    }}
                  >
                    {isDownloading ? <LoadingSpinner size={14} /> : isInstalled ? `✓ ${t('settings.java_downloaded')}` : t('settings.java_download_btn')}
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
                <Button size="sm" variant="ghost" onClick={async () => {
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
            <div className="settings-row__title">{t('settings.show_snapshots_title')}</div>
            <div className="settings-row__desc">
              {t('settings.show_snapshots_desc')}
            </div>
          </div>
          <div className="settings-row__control">
            <label className="toggle">
              <input
                type="checkbox"
                checked={localConfig.show_snapshots}
                onChange={(e) => updateConfig('show_snapshots', e.target.checked)}
              />
              <span className="toggle__slider" />
            </label>
          </div>
        </div>

        <div className="settings-row">
          <div className="settings-row__info">
            <div className="settings-row__title">{t('settings.show_old_versions_title')}</div>
            <div className="settings-row__desc">
              {t('settings.show_old_versions_desc')}
            </div>
          </div>
          <div className="settings-row__control">
            <label className="toggle">
              <input
                type="checkbox"
                checked={localConfig.show_old_versions}
                onChange={(e) => updateConfig('show_old_versions', e.target.checked)}
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
            <Button size="sm" variant="ghost" onClick={async () => {
              await invoke('cmd_open_folder', { path: localConfig.data_dir });
            }}>{t('settings.data_dir_open')}</Button>
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
              variant="ghost"
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
  const typeLabel = typeLabelKey ? t(typeLabelKey) : '';

  return (
    <div className="glass-card" style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-xl)', padding: 'var(--space-xl)' }}>
      {hasAccount ? (
        <>
          <img
            src={`https://mc-heads.net/avatar/${activeUuid}/48`}
            referrerPolicy="no-referrer"
            alt={activeName}
            style={{ width: 48, height: 48, borderRadius: 'var(--radius-md)' }}
          />
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
            background: 'var(--surface-glass)',
            display: 'flex', alignItems: 'center', justifyContent: 'center',
            color: 'var(--text-tertiary)',
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
                fontFamily: 'monospace',
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
                fontFamily: 'monospace',
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
          <Button size="sm" variant="ghost" onClick={refresh} disabled={loading}>
            {loading ? <LoadingSpinner size={14} /> : t('settings.latest_version_check_btn')}
          </Button>
        </div>
      </div>
    </section>
  );
}

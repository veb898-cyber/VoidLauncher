import { useEffect, useState, useCallback } from 'react';
import { useSettingsStore } from '../stores/settingsStore';
import { useAuthStore } from '../stores/authStore';
import { open as openFileDialog } from '@tauri-apps/plugin-dialog';
import { invoke } from '@tauri-apps/api/core';
import { Button } from '../components/ui/Button';
import { addToast } from '../components/ui/Toast';
import { CustomSelect } from '../components/ui/CustomSelect';
import { getRecommendedMemoryMb, snapToMemoryStep } from '../lib/memory';
import { useT, Language } from '../lib/i18n';
import { useLanguageStore } from '../stores/languageStore';

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
  const [localConfig, setLocalConfig] = useState(config);
  const [saveSuccess, setSaveSuccess] = useState(false);

  useEffect(() => {
    loadConfig();
    detectJava();
  }, []);

  useEffect(() => {
    setLocalConfig(config);
  }, [config]);

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
        <div className="glass-card" style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-xl)', padding: 'var(--space-xl)' }}>
          {isLoggedIn && profile ? (
            <>
              <img
                src={`https://mc-heads.net/avatar/${profile.id}/48`}
                referrerPolicy="no-referrer"
                alt={profile.name}
                style={{ width: 48, height: 48, borderRadius: 'var(--radius-md)' }}
              />
              <div style={{ flex: 1 }}>
                <div style={{ fontWeight: 600, fontSize: 'var(--font-size-lg)' }}>{profile.name}</div>
                <div style={{ color: 'var(--text-secondary)', fontSize: 'var(--font-size-sm)' }}>
                  {t('settings.signed_in_as', { id: profile.id.substring(0, 8) })}
                </div>
              </div>
              <button className="btn btn--danger" onClick={logout}>
                {t('settings.sign_out')}
              </button>
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
            <Button size="sm" variant="ghost" onClick={() => {
              addToast(t('settings.clear_cache_not_impl'), 'info');
            }}>{t('settings.clear_cache_btn')}</Button>
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
            <span style={{ color: 'var(--text-tertiary)', fontSize: 'var(--font-size-md)', fontWeight: 400 }}>{t('settings.about_version')}</span>
          </div>
          <p style={{ color: 'var(--text-secondary)', fontSize: 'var(--font-size-sm)' }}>
            {t('settings.about_desc')}
            <br />
            {t('settings.about_disclaimer')}
          </p>
        </div>
      </section>
    </div>
  );
}

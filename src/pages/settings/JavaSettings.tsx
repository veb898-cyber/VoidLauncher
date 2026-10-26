import { useCallback, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { open as openFileDialog } from '@tauri-apps/plugin-dialog';
import { RefreshCw } from 'lucide-react';
import { useSettingsStore, type AppConfig } from '../../stores/settingsStore';
import { useJavaDownloadStore } from '../../stores/javaDownloadStore';
import { Button } from '../../components/ui/Button';
import { CustomSelect } from '../../components/ui/CustomSelect';
import { LoadingSpinner } from '../../components/ui/LoadingSpinner';
import { Tooltip } from '../../components/ui/Tooltip';
import { addToast } from '../../components/ui/Toast';
import { getRecommendedMemoryMb, snapToMemoryStep } from '../../lib/memory';
import { useT } from '../../lib/i18n';
import type { UpdateConfig } from './types';

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

interface JavaSettingsProps {
  config: AppConfig;
  updateConfig: UpdateConfig;
}

export function JavaSettings({ config, updateConfig }: JavaSettingsProps) {
  const t = useT();
  const javaInstallations = useSettingsStore((s) => s.javaInstallations);
  const detectJava = useSettingsStore((s) => s.detectJava);
  const detectSystemRam = useSettingsStore((s) => s.detectSystemRam);
  const javaDownload = useJavaDownloadStore((s) => s.active);

  const [availableJava, setAvailableJava] = useState<AvailableJavaVersion[]>([]);
  const [managedJava, setManagedJava] = useState<ManagedJavaRuntime[]>([]);
  const [checkingJava, setCheckingJava] = useState(false);
  const [javaError, setJavaError] = useState(false);
  const [javaRefreshing, setJavaRefreshing] = useState(false);

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
    detectJava();
    loadJavaVersions();
  }, [detectJava, loadJavaVersions]);

  return (
    <>
      <section className="settings-section">
        <h2 className="settings-section__title">{t('settings.group_java_runtime')}</h2>

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
              value={config.java_path || ''}
              placeholder={t('settings.java_path_placeholder')}
              onChange={(e) => updateConfig('java_path', e.target.value || null)}
              style={{ width: 330, fontSize: 'var(--font-size-xs)' }}
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

        <div className="settings-subsection">
          <div className="settings-subsection__head">
            <span className="settings-subsection__title">{t('settings.java_detected_heading')}</span>
            <Button
              size="sm"
              variant="secondary"
              onClick={async () => {
                setJavaRefreshing(true);
                try {
                  await detectJava();
                } finally {
                  setJavaRefreshing(false);
                }
              }}
              disabled={javaRefreshing}
            >
              <RefreshCw size={14} className={javaRefreshing ? 'icon-spin' : undefined} />
              {t('settings.java_refresh')}
            </Button>
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

      <section className="settings-section">
        <h2 className="settings-section__title">{t('settings.group_performance')}</h2>

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
              value={config.default_memory_mb}
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
              value={config.default_gc_preset}
              options={[
                { value: 'standard', label: t('instance_editor.gc_standard'), description: t('instance_editor.gc_standard_short') },
                { value: 'g1gc', label: t('instance_editor.gc_g1gc'), description: t('instance_editor.gc_g1gc_short') },
                { value: 'zgc', label: t('instance_editor.gc_zgc'), description: t('instance_editor.gc_zgc_req') },
              ]}
              onChange={(v) => updateConfig('default_gc_preset', v)}
            />
          </div>
        </div>
      </section>

      <section className="settings-section">
        <h2 className="settings-section__title">{t('settings.java_download_heading')}</h2>
        <p className="settings-section__desc">
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
        <div className="settings-subsection__title" style={{ marginBottom: 'var(--space-md)' }}>{t('settings.java_downloaded')}</div>
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
    </>
  );
}

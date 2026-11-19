import { useCallback, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { open as openFileDialog } from '@tauri-apps/plugin-dialog';
import { Check, Download } from 'lucide-react';
import { useSettingsStore, type AppConfig } from '../../stores/settingsStore';
import { useJavaDownloadStore } from '../../stores/javaDownloadStore';
import { Button } from '../../components/ui/Button';
import { RefreshIcon } from '../../components/ui/RefreshIcon';
import { CustomSelect } from '../../components/ui/CustomSelect';
import { SnakeSpinner } from '../../components/ui/SnakeSpinner';
import { Tooltip } from '../../components/ui/Tooltip';
import { addToast } from '../../components/ui/Toast';
import {
  MAX_MEMORY_MB,
  MIN_MEMORY_MB,
  getRecommendedMemoryMb,
  getSafeMaxMemoryMb,
  snapToMemoryStep,
} from '../../lib/memory';
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
  const javaDownloads = useJavaDownloadStore((s) => s.active);

  const [availableJava, setAvailableJava] = useState<AvailableJavaVersion[]>([]);
  const [managedJava, setManagedJava] = useState<ManagedJavaRuntime[]>([]);
  const [checkingJava, setCheckingJava] = useState(false);
  const [javaError, setJavaError] = useState(false);
  const [javaRefreshing, setJavaRefreshing] = useState(false);
  const [systemRamMb, setSystemRamMb] = useState<number | null>(null);

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

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const total = await invoke<number>('cmd_detect_system_ram');
        if (!cancelled) setSystemRamMb(total > 0 ? total : null);
      } catch {
        if (!cancelled) setSystemRamMb(null);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  const safeMaxMb = systemRamMb == null ? null : getSafeMaxMemoryMb(systemRamMb);
  const storedLimitMb = Math.min(config.max_memory_mb, MAX_MEMORY_MB);
  const ramKnown = systemRamMb != null;
  const ramInsufficient = ramKnown && safeMaxMb == null;
  const uiLimitMb = safeMaxMb != null
    ? Math.min(storedLimitMb, safeMaxMb)
    : ramInsufficient
      ? Math.min(storedLimitMb, MIN_MEMORY_MB)
      : Math.max(
          MIN_MEMORY_MB,
          Math.min(storedLimitMb, config.default_memory_mb),
        );
  const memoryAboveLimit = config.default_memory_mb > uiLimitMb;
  const canRaiseLimit =
    safeMaxMb != null &&
    safeMaxMb > storedLimitMb &&
    (!memoryAboveLimit || safeMaxMb >= config.default_memory_mb);
  const limitAboveSafe = safeMaxMb != null && storedLimitMb > safeMaxMb;
  const zgcBlocked = Math.min(config.default_memory_mb, uiLimitMb) < 8192;
  const formatGb = (mb: number) => (mb / 1024).toFixed(1).replace(/\.0$/, '');

  return (
    <>
      <section className="settings-section">
        <h2 className="settings-section__title">{t('settings.group_performance')}</h2>

        <div className="settings-row" data-setting-id="default_memory_mb">
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
              max={uiLimitMb}
              step="512"
              value={config.default_memory_mb}
              onChange={(e) => {
                // `max` only constrains the spinner arrows: typed values must
                // be clamped too, or a typo is persisted and handed to the JVM.
                const parsed = parseInt(e.target.value, 10) || 2048;
                updateConfig(
                  'default_memory_mb',
                  Math.min(uiLimitMb, Math.max(2048, parsed)),
                );
              }}
              style={{ width: 100, textAlign: 'center' }}
              id="min-memory-input"
              aria-label={t('settings.memory_title')}
            />
            <span style={{ color: 'var(--text-secondary)', fontSize: 'var(--font-size-sm)' }}>{t('settings.memory_unit')}</span>
            <Tooltip content={t('settings.memory_reset_tooltip')}>
              <button
                className="btn btn--secondary btn--sm"
                onClick={async () => {
                  const totalMb = await detectSystemRam();
                  // Tiered: 4 / 6 / 8 GB depending on total RAM, snapped to
                  // 512 MB step, but never above the current explicit cap.
                  const recommended = snapToMemoryStep(
                    getRecommendedMemoryMb(totalMb),
                  );
                  updateConfig(
                    'default_memory_mb',
                    Math.min(recommended, uiLimitMb),
                  );
                }}
                id="reset-memory-btn"
              >
                {t('settings.memory_reset')}
              </button>
            </Tooltip>
          </div>
        </div>

        <div className="settings-row" style={{ paddingTop: 0 }} data-setting-id="max_memory_mb">
          <div className="settings-row__info">
            <div className="settings-row__desc">
              {t('settings.memory_limit_current', {
                limit: formatGb(storedLimitMb),
              })}
              {safeMaxMb != null && (
                <>
                  {' '}
                  {t('settings.memory_limit_safe', {
                    safe: formatGb(safeMaxMb),
                  })}
                </>
              )}
            </div>
            {memoryAboveLimit && (
              <div className="settings-row__note settings-row__note--active">
                {t('settings.memory_limit_exceeded', {
                  selected: formatGb(config.default_memory_mb),
                  effective: formatGb(uiLimitMb),
                })}
              </div>
            )}
            {limitAboveSafe && (
              <div className="settings-row__note settings-row__note--active">
                {t('settings.memory_limit_above_safe', {
                  limit: formatGb(storedLimitMb),
                  safe: formatGb(safeMaxMb!),
                })}
              </div>
            )}
            {systemRamMb == null && (
              <div className="settings-row__note settings-row__note--locked">
                {t('settings.memory_limit_unknown')}
              </div>
            )}
            {systemRamMb != null && safeMaxMb == null && (
              <div className="settings-row__note settings-row__note--active">
                {t('settings.memory_limit_low_ram')}
              </div>
            )}
          </div>
          <div className="settings-row__control">
            {canRaiseLimit && (
              <Button
                size="sm"
                variant="secondary"
                onClick={() => updateConfig('max_memory_mb', safeMaxMb!)}
                id="raise-memory-limit-btn"
              >
                {t('settings.memory_limit_raise', {
                  safe: formatGb(safeMaxMb),
                })}
              </Button>
            )}
          </div>
        </div>

        <div className="settings-row" data-setting-id="default_gc_preset">
          <div className="settings-row__info">
            <div className="settings-row__title">{t('settings.gc_title')}</div>
            <div className="settings-row__desc">
              {t('settings.gc_desc')}
            </div>
            <div className="settings-row__note settings-row__note--locked">
              {t('settings.gc_zgc_note')}
            </div>
            {config.default_gc_preset === 'zgc' && zgcBlocked && (
              <div className="settings-row__note settings-row__note--active">
                {t('settings.gc_zgc_unavailable')}
              </div>
            )}
          </div>
          <div className="settings-row__control" style={{ width: 320 }}>
            <CustomSelect
              value={config.default_gc_preset}
              ariaLabel={t('settings.gc_title')}
              options={[
                { value: 'standard', label: t('instance_editor.gc_standard'), description: t('instance_editor.gc_standard_short') },
                { value: 'g1gc', label: t('instance_editor.gc_g1gc'), description: t('instance_editor.gc_g1gc_short') },
                {
                  value: 'zgc',
                  label: t('instance_editor.gc_zgc'),
                  description: zgcBlocked
                    ? t('settings.gc_zgc_unavailable_short')
                    : t('instance_editor.gc_zgc_req'),
                  disabled: zgcBlocked,
                },
              ]}
              onChange={(v) => updateConfig('default_gc_preset', v)}
            />
          </div>
        </div>
      </section>

      <section className="settings-section">
        <h2 className="settings-section__title">{t('settings.group_java_runtime')}</h2>

        <div className="settings-row" data-setting-id="java_path">
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
              aria-label={t('settings.java_path_title')}
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

        <div className="settings-subsection" data-setting-id="java_installations">
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
              <RefreshIcon spinning={javaRefreshing} />
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

      <section className="settings-section" data-setting-id="java_downloads">
        <h2 className="settings-section__title">{t('settings.java_download_heading')}</h2>
        <p className="settings-section__desc">
          {t('settings.java_download_desc')}
        </p>

        {/* Available for download */}
        {checkingJava ? (
          <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-sm)', padding: 'var(--space-md)' }}>
            <SnakeSpinner size={16} />
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
              const javaDownload = javaDownloads[jv.major_version] ?? null;
              const isDownloading = javaDownload !== null;
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
                    aria-label={`${t('settings.java_download_btn')} Java ${jv.major_version}`}
                    disabled={isDownloading}
                    loading={isDownloading}
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
                        useJavaDownloadStore.getState().clear(jv.major_version);
                      }
                    }}
                  >
                    {isInstalled
                      ? <><Check size={14} /> {t('settings.java_downloaded')}</>
                      : <><Download size={14} /> {t('settings.java_download_btn')}{isDownloading && javaDownload ? ` ${Math.round(javaDownload.percent)}%` : ''}</>}
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
                <Button size="sm" variant="secondary"
                  aria-label={`${t('settings.java_remove_btn')} Java ${m.major_version}`}
                  onClick={async () => {
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

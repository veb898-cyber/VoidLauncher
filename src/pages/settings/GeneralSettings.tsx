import { useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Button } from '../../components/ui/Button';
import { CustomSelect } from '../../components/ui/CustomSelect';
import { Modal } from '../../components/ui/Modal';
import { Toggle } from '../../components/ui/Toggle';
import { useT } from '../../lib/i18n';
import {
  beginCacheClear,
  canDismissCacheClear,
  failCacheClear,
  finishCacheClear,
  initialCacheClearState,
  type CacheClearState,
} from '../../lib/cacheClear';
import type { AppConfig } from '../../stores/settingsStore';
import { useToastSettingsStore } from '../../stores/toastSettingsStore';
import { useSkinSettingsStore } from '../../stores/skinSettingsStore';
import type { UpdateConfig } from './types';

interface GeneralSettingsProps {
  config: AppConfig;
  updateConfig: UpdateConfig;
}

/**
 * Launcher-wide preferences: what happens around a launch, skin handling,
 * notifications, and the data/cache actions. Visual preferences live in the
 * Appearance category, Java runtime and memory tuning in Java & performance.
 */
export function GeneralSettings({ config, updateConfig }: GeneralSettingsProps) {
  const t = useT();
  const [cacheDialogOpen, setCacheDialogOpen] = useState(false);
  const [cacheState, setCacheState] = useState<CacheClearState>(initialCacheClearState);
  // Synchronous lock: React state alone can leave a one-frame window where a
  // second click starts a parallel deletion before the button is disabled.
  const cacheRunningRef = useRef(false);
  const toastSuccess = useToastSettingsStore((s) => s.success);
  const toastWarning = useToastSettingsStore((s) => s.warning);
  const toastError = useToastSettingsStore((s) => s.error);
  const setToastCategory = useToastSettingsStore((s) => s.setCategory);
  const skinRefreshMode = useSkinSettingsStore((s) => s.refreshMode);
  const setSkinRefreshMode = useSkinSettingsStore((s) => s.setRefreshMode);

  const toastRows = [
    {
      key: 'success' as const,
      checked: toastSuccess,
      title: t('settings.toast_success_title'),
      desc: t('settings.toast_success_desc'),
    },
    {
      key: 'warning' as const,
      checked: toastWarning,
      title: t('settings.toast_warning_title'),
      desc: t('settings.toast_warning_desc'),
    },
    {
      key: 'error' as const,
      checked: toastError,
      title: t('settings.toast_error_title'),
      desc: t('settings.toast_error_desc'),
    },
  ];

  const cacheBusy = cacheState.phase === 'clearing';
  const cacheSuccess = cacheState.phase === 'success';
  const cacheError = cacheState.phase === 'error' ? cacheState.error : null;
  const cacheFreedMb = Math.round((cacheState.freedBytes ?? 0) / 1024 / 1024);

  const openCacheDialog = () => {
    setCacheState(initialCacheClearState);
    setCacheDialogOpen(true);
  };

  const closeCacheDialog = () => {
    if (!canDismissCacheClear(cacheState)) return;
    setCacheDialogOpen(false);
    setCacheState(initialCacheClearState);
  };

  const handleClearCache = async () => {
    if (cacheRunningRef.current) return;
    cacheRunningRef.current = true;
    setCacheState((prev) => beginCacheClear(prev));
    try {
      const freed = await invoke<number>('cmd_clear_cache');
      const freedBytes = Number.isFinite(Number(freed)) ? Number(freed) : 0;
      setCacheState((prev) => finishCacheClear(prev, freedBytes));
    } catch (e) {
      setCacheState((prev) => failCacheClear(prev, String(e)));
    } finally {
      cacheRunningRef.current = false;
    }
  };

  return (
    <>
      <section className="settings-section">
        <h2 className="settings-section__title">{t('settings.group_launch')}</h2>

        <div className="settings-row" data-setting-id="after_launch_action">
          <div className="settings-row__info">
            <div className="settings-row__title">{t('settings.after_launch_title')}</div>
            <div className="settings-row__desc">
              {t('settings.after_launch_desc')}
            </div>
          </div>
          <div className="settings-row__control" style={{ width: 240 }}>
            <CustomSelect
              value={config.after_launch_action}
              ariaLabel={t('settings.after_launch_title')}
              options={[
                { value: 'do_nothing', label: t('settings.after_launch_nothing') },
                { value: 'minimize', label: t('settings.after_launch_minimize') },
                { value: 'close', label: t('settings.after_launch_close') },
              ]}
              onChange={(v) => updateConfig('after_launch_action', v as AppConfig['after_launch_action'])}
            />
          </div>
        </div>
      </section>

      <section className="settings-section">
        <h2 className="settings-section__title">{t('settings.group_skins')}</h2>

        <div className="settings-row" data-setting-id="skin_refresh_mode">
          <div className="settings-row__info">
            <div className="settings-row__title">{t('settings.skin_refresh_title')}</div>
            <div className="settings-row__desc">
              {t('settings.skin_refresh_desc')}
            </div>
          </div>
          <div className="settings-row__control" style={{ width: 240 }}>
            <CustomSelect
              value={skinRefreshMode}
              ariaLabel={t('settings.skin_refresh_title')}
              options={[
                { value: 'auto', label: t('settings.skin_refresh_auto') },
                { value: 'manual', label: t('settings.skin_refresh_manual') },
              ]}
              onChange={(v) => setSkinRefreshMode(v === 'manual' ? 'manual' : 'auto')}
            />
          </div>
        </div>

        <div className="settings-row" data-setting-id="elyby_skins_in_game">
          <div className="settings-row__info">
            <div className="settings-row__title">{t('settings.elyby_skins_title')}</div>
            <div className="settings-row__desc">
              {t('settings.elyby_skins_desc')}
            </div>
            <div
              className="settings-row__desc"
              style={{ marginTop: 6, maxWidth: 720 }}
            >
              {t('settings.elyby_skins_note')}
            </div>
          </div>
          <div className="settings-row__control">
            <Toggle
              checked={config.elyby_skins_in_game}
              ariaLabel={t('settings.elyby_skins_title')}
              onChange={(v) => updateConfig('elyby_skins_in_game', v)}
            />
          </div>
        </div>
      </section>

      <section className="settings-section">
        <h2 className="settings-section__title">{t('settings.group_notifications')}</h2>

        {toastRows.map((row) => (
          <div className="settings-row" key={row.key} data-setting-id={`toast_${row.key}`}>
            <div className="settings-row__info">
              <div className="settings-row__title">{row.title}</div>
              <div className="settings-row__desc">{row.desc}</div>
            </div>
            <div className="settings-row__control">
              <Toggle
                checked={row.checked}
                onChange={(v) => setToastCategory(row.key, v)}
                ariaLabel={row.title}
              />
            </div>
          </div>
        ))}
      </section>

      <section className="settings-section">
        <h2 className="settings-section__title">{t('settings.group_storage')}</h2>

        <div className="settings-row" data-setting-id="data_dir">
          <div className="settings-row__info">
            <div className="settings-row__title">{t('settings.data_dir_title')}</div>
            <div className="settings-row__desc">
              {config.data_dir}
            </div>
          </div>
          <div className="settings-row__control">
            <Button size="sm" variant="secondary" onClick={async () => {
              await invoke('cmd_open_folder', { path: config.data_dir });
            }}>{t('settings.data_dir_open')}</Button>
          </div>
        </div>

        <div className="settings-row" data-setting-id="game_logs">
          <div className="settings-row__info">
            <div className="settings-row__title">{t('settings.game_logs_title')}</div>
            <div className="settings-row__desc">
              {t('settings.game_logs_desc')}
            </div>
          </div>
          <div className="settings-row__control">
            <Button size="sm" variant="secondary" onClick={async () => {
              await invoke('cmd_open_folder', { path: `${config.data_dir}/logs/game` });
            }}>{t('settings.game_logs_open')}</Button>
          </div>
        </div>

        <div className="settings-row" data-setting-id="clear_cache">
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
              aria-haspopup="dialog"
              onClick={openCacheDialog}
            >
              {t('settings.clear_cache_btn')}
            </Button>
          </div>
        </div>
      </section>

      <Modal
        open={cacheDialogOpen}
        onClose={closeCacheDialog}
        dismissible={!cacheBusy}
        title={cacheSuccess
          ? t('settings.clear_cache_success_title')
          : t('settings.clear_cache_confirm_title')}
        maxWidth={540}
        footer={
          cacheSuccess ? (
            <div style={{ display: 'flex', justifyContent: 'flex-end' }}>
              <Button key="cache-success-close" variant="primary" onClick={closeCacheDialog} autoFocus>
                {t('common.close')}
              </Button>
            </div>
          ) : (
            <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 'var(--space-sm)' }}>
              <Button variant="ghost" onClick={closeCacheDialog} disabled={cacheBusy}>
                {t('common.cancel')}
              </Button>
              <Button
                variant="primary"
                onClick={handleClearCache}
                loading={cacheBusy}
                disabled={cacheBusy}
              >
                {cacheError
                  ? t('settings.clear_cache_retry_btn')
                  : t('settings.clear_cache_btn')}
              </Button>
            </div>
          )
        }
      >
        {cacheSuccess ? (
          <div className="cache-clear__success" role="status">
            {t('settings.clear_cache_success', { mb: cacheFreedMb.toString() })}
          </div>
        ) : (
          <>
            <p style={{ margin: 0, color: 'var(--text-secondary)', lineHeight: 1.6 }}>
              {t('settings.clear_cache_confirm_desc')}
            </p>
            <div className="cache-clear__facts">
              <div>
                <div className="cache-clear__fact-label">{t('settings.clear_cache_delete_label')}</div>
                <div className="cache-clear__fact-text">{t('settings.clear_cache_delete_list')}</div>
              </div>
              <div>
                <div className="cache-clear__fact-label">{t('settings.clear_cache_keep_label')}</div>
                <div className="cache-clear__fact-text">{t('settings.clear_cache_keep_list')}</div>
              </div>
            </div>
            <p className="cache-clear__note">{t('settings.clear_cache_confirm_note')}</p>
            {cacheBusy && (
              <div className="cache-clear__progress" role="status">
                {t('settings.clear_cache_in_progress')}
              </div>
            )}
            {cacheError && (
              <div className="cache-clear__error" role="alert">
                {t('settings.clear_cache_error', { error: cacheError })}
              </div>
            )}
          </>
        )}
      </Modal>
    </>
  );
}

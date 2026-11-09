import { useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Button } from '../../components/ui/Button';
import { CustomSelect } from '../../components/ui/CustomSelect';
import { Toggle } from '../../components/ui/Toggle';
import { addToast } from '../../components/ui/Toast';
import { useT } from '../../lib/i18n';
import type { AppConfig } from '../../stores/settingsStore';
import { useToastSettingsStore } from '../../stores/toastSettingsStore';
import type { UpdateConfig } from './types';

interface LauncherSettingsProps {
  config: AppConfig;
  updateConfig: UpdateConfig;
}

export function LauncherSettings({ config, updateConfig }: LauncherSettingsProps) {
  const t = useT();
  const [clearingCache, setClearingCache] = useState(false);
  const toastSuccess = useToastSettingsStore((s) => s.success);
  const toastWarning = useToastSettingsStore((s) => s.warning);
  const toastError = useToastSettingsStore((s) => s.error);
  const setToastCategory = useToastSettingsStore((s) => s.setCategory);

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

  return (
    <>
      <section className="settings-section">
        <h2 className="settings-section__title">{t('settings.group_behavior')}</h2>

        <div className="settings-row">
          <div className="settings-row__info">
            <div className="settings-row__title">{t('settings.after_launch_title')}</div>
            <div className="settings-row__desc">
              {t('settings.after_launch_desc')}
            </div>
          </div>
          <div className="settings-row__control" style={{ width: 240 }}>
            <CustomSelect
              value={config.after_launch_action}
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
        <h2 className="settings-section__title">{t('settings.group_notifications')}</h2>

        {toastRows.map((row) => (
          <div className="settings-row" key={row.key}>
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
        <h2 className="settings-section__title">{t('settings.group_elyby')}</h2>

        <div className="settings-row">
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
              onChange={(v) => updateConfig('elyby_skins_in_game', v)}
            />
          </div>
        </div>
      </section>

      <section className="settings-section">
        <h2 className="settings-section__title">{t('settings.group_storage')}</h2>

        <div className="settings-row">
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

        <div className="settings-row">
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
              loading={clearingCache}
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
              {t('settings.clear_cache_btn')}
            </Button>
          </div>
        </div>
      </section>
    </>
  );
}

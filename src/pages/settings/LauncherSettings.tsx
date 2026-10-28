import { useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Trash2 } from 'lucide-react';
import { Button } from '../../components/ui/Button';
import { Toggle } from '../../components/ui/Toggle';
import { addToast } from '../../components/ui/Toast';
import { useT } from '../../lib/i18n';
import type { AppConfig } from '../../stores/settingsStore';
import type { UpdateConfig } from './types';

interface LauncherSettingsProps {
  config: AppConfig;
  updateConfig: UpdateConfig;
}

export function LauncherSettings({ config, updateConfig }: LauncherSettingsProps) {
  const t = useT();
  const [clearingCache, setClearingCache] = useState(false);

  return (
    <>
      <section className="settings-section">
        <h2 className="settings-section__title">{t('settings.group_behavior')}</h2>

        <div className="settings-row">
          <div className="settings-row__info">
            <div className="settings-row__title">{t('settings.close_on_launch_title')}</div>
            <div className="settings-row__desc">
              {t('settings.close_on_launch_desc')}
            </div>
          </div>
          <div className="settings-row__control">
            <Toggle
              checked={config.close_on_launch}
              onChange={(v) => updateConfig('close_on_launch', v)}
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
              <Trash2 size={14} />
              {t('settings.clear_cache_btn')}
            </Button>
          </div>
        </div>
      </section>
    </>
  );
}

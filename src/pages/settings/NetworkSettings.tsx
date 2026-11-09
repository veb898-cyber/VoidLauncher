import { invoke } from '@tauri-apps/api/core';
import { Toggle } from '../../components/ui/Toggle';
import { useT } from '../../lib/i18n';
import type { AppConfig } from '../../stores/settingsStore';
import type { UpdateConfig } from './types';

interface NetworkSettingsProps {
  config: AppConfig;
  updateConfig: UpdateConfig;
}

export function NetworkSettings({ config, updateConfig }: NetworkSettingsProps) {
  const t = useT();

  /**
   * Apply the proxy policy from the current draft immediately. Persistence
   * still requires the page's Save button, but network behaviour must not wait
   * for it — especially when the parent switch turns the proxy off.
   */
  const manualDisabled = !config.proxy_enabled || config.use_system_proxy;

  const applyPolicy = (patch: Partial<Pick<
    AppConfig,
    'proxy_enabled' | 'proxy_only' | 'proxy_addr' | 'proxy_port' | 'use_system_proxy'
  >>) => {
    const next = { ...config, ...patch };
    void invoke('cmd_apply_proxy_policy', {
      proxyEnabled: next.proxy_enabled,
      useSystemProxy: next.use_system_proxy,
      proxyAddr: next.proxy_addr,
      proxyPort: next.proxy_port,
      proxyOnly: next.proxy_only,
    }).catch(() => {
      // A transient IPC failure must not block editing; Save re-applies the
      // policy through cmd_save_config.
    });
  };

  return (
    <section className="settings-section">
      <h2 className="settings-section__title">{t('settings.group_proxy')}</h2>

      <div className="settings-row">
        <div className="settings-row__info">
          <div className="settings-row__title">{t('settings.proxy_enabled_title')}</div>
          <div className="settings-row__desc">
            {t('settings.proxy_enabled_desc')}
          </div>
        </div>
        <div className="settings-row__control">
          <Toggle
            checked={config.proxy_enabled}
            onChange={(v) => {
              updateConfig('proxy_enabled', v);
              applyPolicy({ proxy_enabled: v });
            }}
            ariaLabel={t('settings.proxy_enabled_title')}
          />
        </div>
      </div>

      <div
        className="settings-row settings-row--dependent"
        style={{ opacity: config.proxy_enabled ? 1 : 0.5 }}
      >
        <div className="settings-row__info">
          <div className="settings-row__title">{t('settings.proxy_system_title')}</div>
          <div className="settings-row__desc">
            {t('settings.proxy_system_desc')}
          </div>
          {!config.proxy_enabled && (
            <div className="settings-row__desc" style={{ marginTop: 4 }}>
              {t('settings.proxy_system_disabled_hint')}
            </div>
          )}
          {config.proxy_enabled && config.use_system_proxy && (
            <div className="settings-row__note">{t('settings.proxy_system_note')}</div>
          )}
        </div>
        <div className="settings-row__control">
          <Toggle
            checked={config.use_system_proxy}
            onChange={(v) => {
              updateConfig('use_system_proxy', v);
              applyPolicy({ use_system_proxy: v });
            }}
            disabled={!config.proxy_enabled}
            ariaLabel={t('settings.proxy_system_title')}
          />
        </div>
      </div>

      <div
        className="settings-row settings-row--stacked"
        style={{ opacity: manualDisabled ? 0.5 : 1 }}
      >
        <div className="settings-row__info">
          <div className="settings-row__title">{t('settings.proxy_server_title')}</div>
          {config.use_system_proxy && (
            <div className="settings-row__desc" style={{ marginTop: 4 }}>
              {t('settings.proxy_manual_ignored_hint')}
            </div>
          )}
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
              value={config.proxy_addr}
              disabled={manualDisabled}
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
              value={config.proxy_port || ''}
              disabled={manualDisabled}
              onChange={(e) => updateConfig('proxy_port', Math.max(1, Math.min(65535, Number(e.target.value) || 0)))}
            />
            <span className="field-group__hint">{t('settings.proxy_port_desc')}</span>
          </div>
        </div>
      </div>

      <div
        className="settings-row settings-row--dependent"
        style={{ opacity: config.proxy_enabled ? 1 : 0.5 }}
      >
        <div className="settings-row__info">
          <div className="settings-row__title">{t('settings.proxy_only_title')}</div>
          <div className="settings-row__desc">
            {t('settings.proxy_only_desc')}
          </div>
          {!config.proxy_enabled && (
            <div className="settings-row__desc" style={{ marginTop: 4 }}>
              {t('settings.proxy_only_disabled_hint')}
            </div>
          )}
        </div>
        <div className="settings-row__control">
          <Toggle
            checked={config.proxy_only}
            onChange={(v) => {
              updateConfig('proxy_only', v);
              applyPolicy({ proxy_only: v });
            }}
            disabled={!config.proxy_enabled}
            ariaLabel={t('settings.proxy_only_title')}
          />
        </div>
      </div>
    </section>
  );
}

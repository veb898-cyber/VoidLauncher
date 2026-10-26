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
            onChange={(v) => updateConfig('proxy_enabled', v)}
          />
        </div>
      </div>

      <div className="settings-row settings-row--stacked" style={{ opacity: config.proxy_enabled ? 1 : 0.5 }}>
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
              value={config.proxy_addr}
              disabled={!config.proxy_enabled}
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
              disabled={!config.proxy_enabled}
              onChange={(e) => updateConfig('proxy_port', Math.max(1, Math.min(65535, Number(e.target.value) || 0)))}
            />
            <span className="field-group__hint">{t('settings.proxy_port_desc')}</span>
          </div>
        </div>
      </div>
    </section>
  );
}

import { FlaskConical } from 'lucide-react';
import { Toggle } from '../../components/ui/Toggle';
import { useT } from '../../lib/i18n';
import type { AppConfig } from '../../stores/settingsStore';
import type { UpdateConfig } from './types';

interface ExperimentalSettingsProps {
  config: AppConfig;
  updateConfig: UpdateConfig;
}

/**
 * Opt-in switches for features that are still being validated. The flags live
 * in the normal AppConfig and are saved with the regular Settings save button;
 * nothing here is a separate settings system.
 */
export function ExperimentalSettings({ config, updateConfig }: ExperimentalSettingsProps) {
  const t = useT();

  return (
    <section className="settings-section">
      <h2 className="settings-section__title">{t('settings.group_experimental')}</h2>
      <p className="settings-section__desc">{t('settings.experimental_intro')}</p>

      <div className="settings-row" data-setting-id="rooms_enabled">
        <div className="settings-row__info">
          <div className="settings-row__title" style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
            <FlaskConical size={16} aria-hidden color="var(--primary)" />
            {t('settings.rooms_experimental_title')}
          </div>
          <div className="settings-row__desc">
            {t('settings.rooms_experimental_desc')}
          </div>
          <div className="settings-row__note">
            {t('settings.rooms_experimental_note')}
          </div>
        </div>
        <div className="settings-row__control">
          <Toggle
            checked={config.rooms_enabled}
            onChange={(value) => updateConfig('rooms_enabled', value)}
            ariaLabel={t('settings.rooms_experimental_title')}
          />
        </div>
      </div>
    </section>
  );
}

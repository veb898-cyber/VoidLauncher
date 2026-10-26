import { openUrl } from '@tauri-apps/plugin-opener';
import { RefreshCw } from 'lucide-react';
import { Button } from '../../components/ui/Button';
import { useT } from '../../lib/i18n';
import {
  APP_VERSION,
  useLatestVersion,
  getVersionComparison,
} from '../../hooks/useLatestVersion';

export function AboutSettings() {
  const t = useT();

  return (
    <>
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
    </>
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
                fontFamily: 'var(--font-ui)',
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
                fontFamily: 'var(--font-ui)',
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
          <Button size="sm" variant="secondary" onClick={refresh} disabled={loading}>
            <RefreshCw size={14} className={loading ? 'icon-spin' : undefined} />
            {t('settings.latest_version_check_btn')}
          </Button>
        </div>
      </div>
    </section>
  );
}

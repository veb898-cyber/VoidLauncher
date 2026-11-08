import { SnakeSpinner } from '../ui/SnakeSpinner';
import { ProgressBar } from '../ui/ProgressBar';
import { useEventStore } from '../../hooks/useGameEvents';
import { useT } from '../../lib/i18n';
import { installDisplay, isTerminalInstallStage } from '../../lib/installProgress';

export function InstallOverlay() {
  const t = useT();
  const progress = useEventStore((s) => s.installProgress);

  const stageLabels: Record<string, string> = {
    manifest: t('install.stage_fetching'),
    libraries: t('install.stage_libraries'),
    assets: t('install.stage_assets'),
    finalizing: t('install.stage_finalizing'),
    java: t('install.stage_java'),
    done: t('install.stage_complete'),
  };

  if (!progress || isTerminalInstallStage(progress.stage)) return null;

  const stageLabel = stageLabels[progress.stage] || progress.stage;
  const display = installDisplay(progress);

  return (
    <div className="install-overlay">
      <div className="install-overlay__sheet">
        <div
          style={{
            display: 'flex',
            alignItems: 'center',
            gap: 'var(--space-lg)',
            marginBottom: 'var(--space-xl)',
          }}
        >
          <div
            style={{
              width: 48,
              height: 48,
              borderRadius: 'var(--radius-lg)',
              background: 'var(--accent-dim)',
              display: 'flex',
              alignItems: 'center',
              justifyContent: 'center',
            }}
          >
            <SnakeSpinner size={24} />
          </div>
          <div>
            <div style={{ fontWeight: 600, fontSize: 'var(--font-size-lg)' }}>
              {t('common.installing')}
            </div>
            <div style={{ color: 'var(--text-secondary)', fontSize: 'var(--font-size-sm)' }}>
              {progress.instance_id || 'Minecraft'}
            </div>
          </div>
        </div>

        <div style={{ marginBottom: 'var(--space-lg)' }}>
          <ProgressBar percent={display.percent} indeterminate={display.indeterminate} />
        </div>

        <div
          style={{
            display: 'flex',
            justifyContent: 'space-between',
            fontSize: 'var(--font-size-sm)',
            color: 'var(--text-secondary)',
            marginBottom: 'var(--space-sm)',
          }}
        >
          <span>{stageLabel}</span>
          {display.showPercent && <span>{Math.round(display.percent)}%</span>}
        </div>

        {display.showBytes && (
          <div
            style={{
              fontSize: 'var(--font-size-xs)',
              color: 'var(--text-tertiary)',
            }}
          >
            {t('install.progress', {
              prepared: display.preparedMb,
              total: display.totalMb,
            })}
            {display.showInProgress && (
              <>
                {' · '}
                {t('install.in_progress', { inProgress: display.inProgressMb })}
              </>
            )}
            {display.showNetworkBytes && (
              <>
                {' · '}
                {t('install.network_progress', { network: display.networkMb })}
              </>
            )}
          </div>
        )}

        {progress.message && (
          <div
            style={{
              fontSize: 'var(--font-size-xs)',
              color: 'var(--text-tertiary)',
              marginTop: 'var(--space-sm)',
            }}
          >
            {progress.message}
          </div>
        )}
      </div>
    </div>
  );
}

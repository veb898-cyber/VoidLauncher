import { Loader2 } from 'lucide-react';
import { ProgressBar } from '../ui/ProgressBar';
import { useEventStore } from '../../hooks/useGameEvents';
import { useT } from '../../lib/i18n';

export function InstallOverlay() {
  const t = useT();
  const progress = useEventStore((s) => s.installProgress);

  const stageLabels: Record<string, string> = {
    manifest: t('install.stage_fetching'),
    libraries: t('install.stage_libraries'),
    assets: t('install.stage_assets'),
    java: t('install.stage_java'),
    done: t('install.stage_complete'),
  };

  if (!progress || progress.stage === 'done') return null;

  const stageLabel = stageLabels[progress.stage] || progress.stage;
  const displayMb = progress.total_bytes > 0;
  const downloadedMb = (progress.downloaded_bytes / 1024 / 1024).toFixed(1);
  const totalMb = (progress.total_bytes / 1024 / 1024).toFixed(1);

  return (
    <div
      style={{
        position: 'fixed',
        inset: 0,
        background: 'rgba(0, 0, 0, 0.7)',
        backdropFilter: 'blur(12px)',
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'center',
        zIndex: 1500,
        animation: 'fadeIn 0.2s ease-out',
      }}
    >
      <div
        style={{
          background: 'var(--bg-secondary)',
          border: '1px solid var(--surface-border)',
          borderRadius: 'var(--radius-xl)',
          padding: 'var(--space-3xl)',
          width: '90%',
          maxWidth: 480,
          boxShadow: 'var(--shadow-xl)',
        }}
      >
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
            <Loader2 size={24} style={{ animation: 'spin 1s linear infinite' }} />
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
          <ProgressBar percent={progress.percent} />
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
          <span>{Math.round(progress.percent)}%</span>
        </div>

        {displayMb && (
          <div
            style={{
              fontSize: 'var(--font-size-xs)',
              color: 'var(--text-tertiary)',
            }}
          >
            {t('install.progress', { downloaded: downloadedMb, total: totalMb })}
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

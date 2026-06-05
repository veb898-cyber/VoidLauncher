import { Plus, Package } from 'lucide-react';
import { useInstanceStore } from '../../stores/instanceStore';
import { t } from '../../lib/i18n';
import { Button } from '../ui/Button';
import { ProgressBar } from '../ui/ProgressBar';
import { useEventStore } from '../../hooks/useGameEvents';

interface InstanceListProps {
  onCreateClick: () => void;
}

export function InstanceList({ onCreateClick }: InstanceListProps) {
  const instances = useInstanceStore((s) => s.instances);
  const selectedInstance = useInstanceStore((s) => s.selectedInstance);
  const selectInstance = useInstanceStore((s) => s.selectInstance);
  const isLaunching = useInstanceStore((s) => s.isLaunching);
  const installProgress = useEventStore((s) => s.installProgress);

  const installingName = installProgress?.instance_id || null;

  return (
    <div
      style={{
        width: 260,
        borderRight: '1px solid var(--surface-border)',
        display: 'flex',
        flexDirection: 'column',
        background: 'var(--bg-secondary)',
        flexShrink: 0,
      }}
    >
      {/* Header */}
      <div
        style={{
          padding: 'var(--space-lg)',
          borderBottom: '1px solid var(--surface-border)',
          display: 'flex',
          alignItems: 'center',
          justifyContent: 'space-between',
        }}
      >
        <span style={{ fontWeight: 600, fontSize: 'var(--font-size-sm)', textTransform: 'uppercase', letterSpacing: '0.5px', color: 'var(--text-secondary)' }}>
          {t('instances.page_title')}
        </span>
        <Button size="sm" onClick={onCreateClick}>
          <Plus size={14} />
        </Button>
      </div>

      {/* List */}
      <div style={{ flex: 1, overflowY: 'auto', padding: 'var(--space-sm)' }}>
        {instances.length === 0 ? (
          <div
            style={{
              padding: 'var(--space-xl)',
              textAlign: 'center',
              color: 'var(--text-tertiary)',
              fontSize: 'var(--font-size-sm)',
            }}
          >
            <Package size={24} style={{ opacity: 0.3, marginBottom: 'var(--space-sm)' }} />
            <div>{t('instances.empty_title')}</div>
          </div>
        ) : (
          instances.map((instance) => {
            const isInstalling = installingName === instance.name;
            const isSelected = selectedInstance === instance.name;

            return (
              <div
                key={instance.name}
                onClick={() => selectInstance(instance.name)}
                style={{
                  display: 'flex',
                  alignItems: 'center',
                  gap: 'var(--space-md)',
                  padding: 'var(--space-md)',
                  borderRadius: 'var(--radius-md)',
                  background: isSelected ? 'var(--accent-dim)' : 'transparent',
                  cursor: 'pointer',
                  transition: 'background var(--transition-fast)',
                  marginBottom: 2,
                  opacity: isLaunching && !isSelected ? 0.6 : 1,
                }}
                onMouseEnter={(e) => {
                  if (!isSelected) e.currentTarget.style.background = 'var(--surface-glass-hover)';
                }}
                onMouseLeave={(e) => {
                  if (!isSelected) e.currentTarget.style.background = 'transparent';
                }}
              >
                <div
                  style={{
                    width: 36,
                    height: 36,
                    borderRadius: 'var(--radius-md)',
                    background: 'var(--surface-glass)',
                    display: 'flex',
                    alignItems: 'center',
                    justifyContent: 'center',
                    flexShrink: 0,
                    fontSize: 'var(--font-size-lg)',
                  }}
                >
                  <Package size={18} color="var(--accent)" />
                </div>
                <div style={{ flex: 1, minWidth: 0 }}>
                  <div
                    style={{
                      fontSize: 'var(--font-size-sm)',
                      fontWeight: 500,
                      overflow: 'hidden',
                      textOverflow: 'ellipsis',
                      whiteSpace: 'nowrap',
                    }}
                  >
                    {instance.name}
                  </div>
                  <div
                    style={{
                      fontSize: 'var(--font-size-xs)',
                      color: 'var(--text-tertiary)',
                    }}
                  >
                    {instance.loader !== 'Vanilla' ? `${instance.loader} ` : ''}
                    {instance.mc_version}
                  </div>
                  {isInstalling && (
                    <div style={{ marginTop: 4 }}>
                      <ProgressBar percent={installProgress?.percent || 0} />
                    </div>
                  )}
                </div>
              </div>
            );
          })
        )}
      </div>
    </div>
  );
}

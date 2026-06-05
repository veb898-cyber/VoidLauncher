import { useEffect, useMemo } from 'react';
import { Play, Plus, Package, Settings } from 'lucide-react';
import { useAuthStore } from '../stores/authStore';
import { useInstanceStore } from '../stores/instanceStore';
import { LoadingSpinner } from '../components/ui/LoadingSpinner';
import { useT, formatPlayTime } from '../lib/i18n';

interface HomeProps {
  onNavigate: (page: string) => void;
}

export function Home({ onNavigate }: HomeProps) {
  const t = useT();
  const { profile, isLoggedIn } = useAuthStore();
  const instances = useInstanceStore((s) => s.instances);
  const isLaunching = useInstanceStore((s) => s.isLaunching);
  const launchGame = useInstanceStore((s) => s.launchGame);
  const selectInstance = useInstanceStore((s) => s.selectInstance);
  const loadInstances = useInstanceStore((s) => s.loadInstances);

  useEffect(() => {
    loadInstances();
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const lastInstance = useMemo(() => {
    if (instances.length === 0) return null;
    const sorted = [...instances].sort((a, b) => {
      const aTime = a.last_played || '';
      const bTime = b.last_played || '';
      return bTime.localeCompare(aTime);
    });
    return sorted[0];
  }, [instances]);

  const handleQuickPlay = async () => {
    if (!isLoggedIn) {
      onNavigate('login');
      return;
    }
    if (lastInstance) {
      await launchGame(lastInstance.name);
    } else {
      onNavigate('instances');
    }
  };

  return (
    <div className="page animate-fade-in" style={{ display: 'flex', flexDirection: 'column', gap: 'var(--space-xl)' }}>
      {/* Top bar: greeting + quick play */}
      <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-lg)' }}>
        {isLoggedIn && profile ? (
          <img
            src={`https://mc-heads.net/avatar/${profile.id}/56`}
            referrerPolicy="no-referrer"
            alt={profile.name}
            style={{ width: 56, height: 56, borderRadius: 'var(--radius-lg)' }}
          />
        ) : (
          <div style={{
            width: 56, height: 56,
            borderRadius: 'var(--radius-lg)',
            background: 'var(--surface-glass)',
            display: 'flex', alignItems: 'center', justifyContent: 'center',
            fontSize: 24, color: 'var(--text-tertiary)',
          }}>
            ?
          </div>
        )}
        <div style={{ flex: 1 }}>
          <div style={{ fontSize: 'var(--font-size-xl)', fontWeight: 700 }}>
            {isLoggedIn && profile ? t('home.greeting_logged_in', { name: profile.name }) : t('home.greeting_guest')}
          </div>
          <div style={{ color: 'var(--text-secondary)', fontSize: 'var(--font-size-sm)' }}>
            {isLoggedIn
              ? (lastInstance ? t('home.subtitle_last_played', { name: lastInstance.name }) : t('home.subtitle_no_instances'))
              : t('home.subtitle_not_logged_in')}
          </div>
        </div>
        <div style={{ display: 'flex', gap: 'var(--space-sm)' }}>
          <button
            className="btn btn--play"
            onClick={handleQuickPlay}
            disabled={isLaunching}
            id="quick-play-btn"
            style={{ padding: 'var(--space-md) var(--space-xl)' }}
          >
            {isLaunching ? (
              <><LoadingSpinner size={18} /> {t('home.btn_launching')}</>
            ) : !isLoggedIn ? (
              <><Play size={18} /> {t('home.btn_login')}</>
            ) : lastInstance ? (
              <><Play size={18} fill="currentColor" /> {t('home.btn_play')}</>
            ) : (
              <><Plus size={18} /> {t('home.btn_create')}</>
            )}
          </button>
          <button
            className="btn btn--ghost"
            onClick={() => onNavigate('settings')}
            title={t('home.settings_title')}
          >
            <Settings size={18} />
          </button>
        </div>
      </div>

      {/* Instance grid */}
      <div style={{ flex: 1 }}>
        <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', marginBottom: 'var(--space-md)' }}>
          <h2 style={{ fontSize: 'var(--font-size-lg)', fontWeight: 600 }}>
            {instances.length > 0 ? t('home.section_instances') : ''}
          </h2>
          {instances.length > 0 && (
            <button
              className="btn btn--ghost btn--sm"
              onClick={() => onNavigate('instances')}
            >
              {t('home.view_all')}
            </button>
          )}
        </div>

        {instances.length === 0 ? (
          <div style={{
            flex: 1,
            display: 'flex', flexDirection: 'column',
            alignItems: 'center', justifyContent: 'center',
            color: 'var(--text-tertiary)',
            gap: 'var(--space-md)',
            padding: 'var(--space-3xl)',
            borderRadius: 'var(--radius-lg)',
            border: '1px dashed var(--border)',
          }}>
            <Package size={48} opacity={0.3} />
            <div style={{ fontSize: 'var(--font-size-lg)', fontWeight: 500 }}>{t('home.empty_title')}</div>
            <div style={{ fontSize: 'var(--font-size-sm)', textAlign: 'center', maxWidth: 300 }}>
              {t('home.empty_desc')}
            </div>
            <button
              className="btn btn--primary"
              onClick={() => onNavigate('instances')}
            >
              <Plus size={16} /> {t('home.empty_cta')}
            </button>
          </div>
        ) : (
          <div className="instance-grid" style={{ display: 'flex', flexDirection: 'column', gap: 'var(--space-sm)' }}>
            {instances.map((instance) => (
              <div
                key={instance.name}
                className="glass-card"
                style={{
                  display: 'flex', alignItems: 'center',
                  gap: 'var(--space-lg)',
                  padding: 'var(--space-md) var(--space-lg)',
                  cursor: 'pointer',
                  transition: 'background var(--transition-fast)',
                }}
                onClick={() => {
                  selectInstance(instance.name);
                  onNavigate('instances');
                }}
                onMouseEnter={(e) => e.currentTarget.style.background = 'var(--accent-dim)'}
                onMouseLeave={(e) => e.currentTarget.style.background = ''}
              >
                <div style={{
                  width: 44, height: 44,
                  borderRadius: 'var(--radius-md)',
                  background: 'var(--surface-glass)',
                  display: 'flex', alignItems: 'center', justifyContent: 'center',
                  flexShrink: 0,
                }}>
                  <Package size={22} color="var(--accent)" />
                </div>
                <div style={{ flex: 1, minWidth: 0 }}>
                  <div style={{ fontWeight: 600, fontSize: 'var(--font-size-md)' }}>
                    {instance.name}
                  </div>
                  <div style={{ display: 'flex', gap: 'var(--space-sm)', alignItems: 'center', fontSize: 'var(--font-size-xs)', color: 'var(--text-secondary)', marginTop: 2 }}>
                    <span style={{
                      padding: '1px 6px',
                      borderRadius: 4,
                      fontSize: 'var(--font-size-xs)',
                      background: instance.loader === 'Vanilla' ? 'var(--surface-glass)' : 'var(--accent-dim)',
                      color: 'var(--text-secondary)',
                    }}>
                      {instance.loader}
                    </span>
                    <span>{instance.mc_version}</span>
                    {instance.last_played && (
                      <>
                        <span style={{ color: 'var(--text-tertiary)' }}>•</span>
                        <span>{new Date(instance.last_played).toLocaleDateString()}</span>
                      </>
                    )}
                    {instance.play_time_seconds > 0 && (
                      <>
                        <span style={{ color: 'var(--text-tertiary)' }}>•</span>
                        <span>{formatPlayTime(instance.play_time_seconds)}</span>
                      </>
                    )}
                  </div>
                </div>
                <button
                  className="btn btn--primary btn--sm"
                  onClick={async (e) => {
                    e.stopPropagation();
                    if (!isLoggedIn) {
                      onNavigate('login');
                      return;
                    }
                    await launchGame(instance.name);
                  }}
                  disabled={isLaunching}
                  style={{ flexShrink: 0 }}
                >
                  {isLaunching ? <LoadingSpinner size={14} /> : <Play size={14} fill="currentColor" />}
                </button>
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

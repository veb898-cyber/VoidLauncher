import { useEffect, useMemo } from 'react';
import { Play, Plus, Package, Settings, Clock } from 'lucide-react';
import { useAuthStore } from '../stores/authStore';
import { useAccountsStore } from '../stores/accountsStore';
import { useInstanceStore } from '../stores/instanceStore';
import { LoadingSpinner } from '../components/ui/LoadingSpinner';
import { useT, formatPlayTime, formatRelativeTime } from '../lib/i18n';

interface HomeProps {
  onNavigate: (page: string) => void;
}

export function Home({ onNavigate }: HomeProps) {
  const t = useT();
  const { profile, isLoggedIn } = useAuthStore();
  const accounts = useAccountsStore((s) => s.accounts);
  const loadAccounts = useAccountsStore((s) => s.loadAccounts);
  const instances = useInstanceStore((s) => s.instances);
  const isLaunching = useInstanceStore((s) => s.isLaunching);
  const launchGame = useInstanceStore((s) => s.launchGame);
  const loadInstances = useInstanceStore((s) => s.loadInstances);
  const selectInstance = useInstanceStore((s) => s.selectInstance);

  useEffect(() => {
    loadInstances();
    loadAccounts();
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

  // Resolve the "active player" for the home greeting:
  //   1. Default account from the accounts list (any type: Microsoft / ElyBy / Offline)
  //   2. Fall back to the legacy Microsoft profile from authStore
  //   3. Otherwise: guest
  const defaultAccount = useMemo(
    () => accounts.find((a) => a.default) ?? null,
    [accounts]
  );

  const activeName = defaultAccount?.name ?? profile?.name ?? null;
  const activeId = defaultAccount?.uuid ?? profile?.id ?? null;
  const hasAccount = !!defaultAccount || isLoggedIn;

  const handleQuickPlay = async () => {
    if (!hasAccount) {
      onNavigate('accounts');
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
        {activeId ? (
          <img
            src={`https://mc-heads.net/avatar/${activeId}/56`}
            referrerPolicy="no-referrer"
            alt={activeName ?? ''}
            style={{ width: 56, height: 56, borderRadius: 'var(--radius-md)' }}
          />
        ) : (
          <div style={{
            width: 56, height: 56,
            borderRadius: 'var(--radius-md)',
            background: 'var(--surface-glass)',
            display: 'flex', alignItems: 'center', justifyContent: 'center',
            fontSize: 24, color: 'var(--text-tertiary)',
          }}>
            ?
          </div>
        )}
        <div style={{ flex: 1 }}>
          <div style={{ fontSize: 'var(--font-size-xl)', fontWeight: 700 }}>
            {activeName
              ? t('home.greeting_logged_in', { name: activeName })
              : t('home.greeting_guest')}
          </div>
          <div style={{ color: 'var(--text-secondary)', fontSize: 'var(--font-size-sm)' }}>
            {!hasAccount
              ? t('home.subtitle_not_logged_in')
              : lastInstance
                ? t('home.subtitle_last_played', { name: lastInstance.name })
                : t('home.subtitle_no_instances')}
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
            ) : !hasAccount ? (
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
          <div className="instance-grid">
            {instances.map((inst) => (
              <div
                key={inst.name}
                className="instance-card"
                onClick={() => {
                  selectInstance(inst.name);
                  onNavigate('instances');
                }}
                role="button"
                tabIndex={0}
              >
                <div className="instance-card__banner">
                  <div className="instance-card__banner-overlay"></div>
                  {inst.icon ? (
                    <img
                      className="instance-card__banner-img"
                      src={inst.icon}
                      alt={inst.name}
                    />
                  ) : (
                    <div className="instance-card__banner-fallback">
                      <Package size={32} />
                    </div>
                  )}
                  {inst.last_played && (
                    <div className="instance-card__last-played">
                      <Clock size={12} />
                      <span>{formatRelativeTime(inst.last_played)}</span>
                    </div>
                  )}
                </div>
                <div className="instance-card__body instance-card__body--horizontal">
                  <div className="instance-card__icon">
                    {inst.icon ? (
                      <img
                        src={inst.icon}
                        alt={inst.name}
                      />
                    ) : (
                      <div className="instance-card__icon-fallback">
                        <Package size={20} />
                      </div>
                    )}
                  </div>
                  <div className="instance-card__info">
                    <div className="instance-card__name">{inst.name}</div>
                    <div className="instance-card__meta">
                      <span className="instance-card__version">{inst.mc_version}</span>
                      {inst.loader && inst.loader !== 'Vanilla' && (
                        <span className={`instance-card__tag instance-card__tag--${inst.loader.toLowerCase()}`}>
                          {inst.loader}
                          {inst.loader_version ? ` ${inst.loader_version}` : ''}
                        </span>
                      )}
                    </div>
                  </div>
                </div>
                <div className="instance-card__actions">
                  <span className="instance-card__playtime">
                    <Clock size={12} />
                    {inst.play_time_seconds && inst.play_time_seconds > 0
                      ? formatPlayTime(inst.play_time_seconds)
                      : t('home.instance_never_played')}
                  </span>
                  <button
                    className="instance-card__play"
                    onClick={(e) => {
                      e.stopPropagation();
                      launchGame(inst.name);
                    }}
                    title={t('home.instance_play_btn')}
                    disabled={isLaunching}
                  >
                    <Play size={14} fill="currentColor" />
                    <span>{t('home.instance_play_btn')}</span>
                  </button>
                </div>
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

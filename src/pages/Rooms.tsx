import { useEffect, useState, type ReactNode } from 'react';
import { Check, CheckCircle2, Copy, Network, Users } from 'lucide-react';
import { useRoomStore, type RoomStatus } from '../stores/roomStore';
import { useInstanceStore } from '../stores/instanceStore';
import { useRoomEvents } from '../hooks/useRoomEvents';
import { RoomSetupView } from '../components/rooms/RoomSetupView';
import { RoomHostView } from '../components/rooms/RoomHostView';
import { RoomGuestView } from '../components/rooms/RoomGuestView';
import { RoomStatusBadge } from '../components/rooms/RoomStatusBadge';
import { Button } from '../components/ui/Button';
import { Input } from '../components/ui/Input';
import { LoadingSpinner } from '../components/ui/LoadingSpinner';
import { addToast } from '../components/ui/Toast';
import { useT } from '../lib/i18n';

// "Комнаты": host a world or join a friend's room over Tailscale Machine
// Sharing. The page orchestrates the room snapshot and delegates each state to
// a dedicated view (setup / host / guest).
export function Rooms() {
  const t = useT();
  useRoomEvents();

  const status = useRoomStore((s) => s.status);
  const loaded = useRoomStore((s) => s.loaded);
  const error = useRoomStore((s) => s.error);
  const clearError = useRoomStore((s) => s.clearError);
  const progress = useRoomStore((s) => s.progress);
  const busy = useRoomStore((s) => s.busy);
  const loadStatus = useRoomStore((s) => s.loadStatus);
  const checkLink = useRoomStore((s) => s.checkLink);

  const instances = useInstanceStore((s) => s.instances);
  const loadInstances = useInstanceStore((s) => s.loadInstances);

  // Command errors go through the same global toast as everywhere else in
  // the launcher (no inline banner). Background install failures arrive via
  // the `room_error` event (see useRoomEvents).
  useEffect(() => {
    if (error) {
      addToast(t('rooms.error_prefix', { error }), 'error');
      clearError();
    }
  }, [error, clearError, t]);

  useEffect(() => {
    loadStatus();
    if (useInstanceStore.getState().instances.length === 0) {
      loadInstances();
    }
  }, [loadStatus, loadInstances]);

  const tsReady = !!status && status.tailscale.installed && status.tailscale.logged_in;

  // Poll while the Tailscale setup is incomplete: the backend install/login
  // tasks run in the background and there is no push event for "ready".
  useEffect(() => {
    if (!status || tsReady) return;
    const id = window.setInterval(() => { loadStatus(); }, 2500);
    return () => window.clearInterval(id);
  }, [status, tsReady, loadStatus]);

  return (
    <div className="page animate-fade-in" style={{ display: 'flex', flexDirection: 'column', minHeight: '100%' }}>
      <div
        className="page__header"
        style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'flex-start', gap: 'var(--space-md)', flexWrap: 'wrap' }}
      >
        <div>
          <h1 className="page__title">{t('rooms.title')}</h1>
          <p className="page__subtitle">{t('rooms.subtitle')}</p>
        </div>
        <RoomStatusBadge status={status} />
      </div>

      {!loaded && !status ? (
        <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-sm)', color: 'var(--text-secondary)' }}>
          <LoadingSpinner /> {t('rooms.loading')}
        </div>
      ) : !status ? (
        <div className="page-stage">
          <div><Button onClick={() => { loadStatus(); }}>{t('rooms.retry')}</Button></div>
        </div>
      ) : !tsReady ? (
        <div className="page-stage">
          <RoomSetupView status={status} progress={progress} busy={busy} onCheck={checkLink} />
        </div>
      ) : status.role === 'host' ? (
        <div className="page-stage">
          <RoomHostView status={status} instances={instances} />
        </div>
      ) : status.role === 'guest' ? (
        <div className="page-stage">
          <RoomGuestView status={status} instances={instances} />
        </div>
      ) : (
        <div className="page-stage">
          <RoomLobby />
        </div>
      )}
    </div>
  );
}

/** No active room: choose between creating a room and joining one. */
function RoomLobby() {
  const t = useT();
  const busy = useRoomStore((s) => s.busy);
  const createHost = useRoomStore((s) => s.createHost);
  const joinGuest = useRoomStore((s) => s.joinGuest);
  const status = useRoomStore((s) => s.status);

  const [name, setName] = useState('');
  const [code, setCode] = useState('');

  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--space-xl)', width: '100%', maxWidth: 860, margin: '0 auto' }}>
      <div
        style={{
          display: 'grid',
          gridTemplateColumns: 'repeat(auto-fit, minmax(300px, 1fr))',
          gap: 'var(--space-xl)',
        }}
      >
        <div className="glass-card animate-slide-up" style={{ display: 'flex', flexDirection: 'column' }}>
          <Users size={26} color="var(--primary)" style={{ marginBottom: 'var(--space-md)' }} />
          <h2 style={{ margin: '0 0 var(--space-sm)', fontSize: 'var(--font-size-lg)' }}>{t('rooms.lobby_create_title')}</h2>
          <p style={{ margin: '0 0 var(--space-lg)', color: 'var(--text-secondary)', lineHeight: 1.5 }}>
            {t('rooms.lobby_create_desc')}
          </p>
          <div style={{ marginTop: 'auto' }}>
            <Input
              id="room-name"
              label={t('rooms.room_name_label')}
              placeholder={t('rooms.room_name_placeholder')}
              value={name}
              maxLength={40}
              onChange={(e) => setName(e.target.value)}
            />
            <Button
              onClick={() => { createHost(name); }}
              loading={busy}
              disabled={busy}
              style={{ width: '100%', marginTop: 'var(--space-md)' }}
            >
              {t('rooms.create_btn')}
            </Button>
          </div>
        </div>

        <div className="glass-card animate-slide-up" style={{ display: 'flex', flexDirection: 'column' }}>
          <Users size={26} color="var(--secondary)" style={{ marginBottom: 'var(--space-md)' }} />
          <h2 style={{ margin: '0 0 var(--space-sm)', fontSize: 'var(--font-size-lg)' }}>{t('rooms.lobby_join_title')}</h2>
          <p style={{ margin: '0 0 var(--space-lg)', color: 'var(--text-secondary)', lineHeight: 1.5 }}>
            {t('rooms.lobby_join_desc')}
          </p>
          <div style={{ marginTop: 'auto' }}>
            <Input
              id="room-code"
              label={t('rooms.room_code_label')}
              placeholder={t('rooms.room_code_placeholder')}
              value={code}
              maxLength={9}
              onChange={(e) => setCode(e.target.value.toUpperCase())}
              onKeyDown={(e) => { if (e.key === 'Enter' && code.trim()) joinGuest(code); }}
            />
            <p style={{ margin: 'var(--space-xs) 0 var(--space-sm)', fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)' }}>
              {t('rooms.room_code_hint')}
            </p>
            <Button
              variant="secondary"
              onClick={() => { joinGuest(code); }}
              loading={busy}
              disabled={busy || !code.trim()}
              style={{ width: '100%' }}
            >
              {t('rooms.join_btn')}
            </Button>
          </div>
        </div>
      </div>

      <RoomLobbyReady status={status} />
      <RoomLobbyHowTo />
    </div>
  );
}

/**
 * What the tailnet connection actually looks like right now. The setup screen
 * only shows this while Tailscale is NOT ready, so on the lobby it is the one
 * place where the user can see which account and address their room will use.
 */
function RoomLobbyReady({ status }: { status: RoomStatus | null }) {
  const t = useT();
  const [copied, setCopied] = useState(false);
  const ts = status?.tailscale;
  if (!ts) return null;

  const copyAddress = async () => {
    if (!ts.self_ip) return;
    try {
      await navigator.clipboard.writeText(ts.self_ip);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } catch {
      addToast(t('common.copy_failed'), 'error');
    }
  };

  const cell = (label: string, value: ReactNode) => (
    <div style={{ minWidth: 0 }}>
      <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)', textTransform: 'uppercase', letterSpacing: '0.06em', marginBottom: 4 }}>
        {label}
      </div>
      <div style={{ fontSize: 'var(--font-size-sm)', color: 'var(--text-primary)', fontWeight: 600, overflow: 'hidden', textOverflow: 'ellipsis' }}>
        {value}
      </div>
    </div>
  );

  return (
    <div className="glass-card animate-slide-up" style={{ padding: 'var(--space-lg) var(--space-xl)' }}>
      <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-sm)', marginBottom: 'var(--space-lg)' }}>
        <CheckCircle2 size={18} color="var(--success)" style={{ flexShrink: 0 }} />
        <span style={{ fontSize: 'var(--font-size-sm)', fontWeight: 600 }}>{t('rooms.lobby_ready_title')}</span>
      </div>
      <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(180px, 1fr))', gap: 'var(--space-lg)' }}>
        {cell(t('rooms.lobby_ready_network'), ts.version || '—')}
        {cell(t('rooms.lobby_ready_account'), ts.login_name || '—')}
        <div style={{ minWidth: 0 }}>
          <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)', textTransform: 'uppercase', letterSpacing: '0.06em', marginBottom: 4 }}>
            {t('rooms.lobby_ready_address')}
          </div>
          <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-sm)' }}>
            <span style={{ fontFamily: 'var(--font-mono)', fontSize: 'var(--font-size-sm)', color: 'var(--text-primary)', fontWeight: 600, overflow: 'hidden', textOverflow: 'ellipsis' }}>
              {ts.self_ip || '—'}
            </span>
            {ts.self_ip && (
              <button
                className="btn btn--ghost btn--sm"
                onClick={copyAddress}
                title={t('rooms.lobby_ready_copy')}
                aria-label={t('rooms.lobby_ready_copy')}
                style={{ flexShrink: 0, padding: '2px 6px' }}
              >
                {copied ? <Check size={14} color="var(--success)" /> : <Copy size={14} />}
              </button>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}

/** Short reminder of the host flow. The detailed Tailscale sharing guide stays
    in RoomShareHelpModal, which can only be opened once a room (and a code)
    exists. */
function RoomLobbyHowTo() {
  const t = useT();
  const steps = [
    { title: t('rooms.lobby_step1_title'), desc: t('rooms.lobby_step1_desc') },
    { title: t('rooms.lobby_step2_title'), desc: t('rooms.lobby_step2_desc') },
    { title: t('rooms.lobby_step3_title'), desc: t('rooms.lobby_step3_desc') },
  ];

  return (
    <div className="glass-card animate-slide-up" style={{ padding: 'var(--space-lg) var(--space-xl)' }}>
      <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-sm)', marginBottom: 'var(--space-lg)' }}>
        <Network size={18} color="var(--primary)" style={{ flexShrink: 0 }} />
        <span style={{ fontSize: 'var(--font-size-sm)', fontWeight: 600 }}>{t('rooms.lobby_how_title')}</span>
      </div>
      <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(200px, 1fr))', gap: 'var(--space-lg)' }}>
        {steps.map((s, i) => (
          <div key={s.title} style={{ display: 'flex', gap: 'var(--space-md)', minWidth: 0 }}>
            <span
              aria-hidden
              style={{
                flexShrink: 0,
                width: 22,
                height: 22,
                borderRadius: 'var(--radius-full)',
                background: 'var(--primary-dim)',
                color: 'var(--primary)',
                fontSize: 'var(--font-size-xs)',
                fontWeight: 700,
                display: 'inline-flex',
                alignItems: 'center',
                justifyContent: 'center',
              }}
            >
              {i + 1}
            </span>
            <div style={{ minWidth: 0 }}>
              <div style={{ fontSize: 'var(--font-size-sm)', fontWeight: 600, marginBottom: 2 }}>{s.title}</div>
              <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-secondary)', lineHeight: 1.5 }}>{s.desc}</div>
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}

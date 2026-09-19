import { useEffect, useState } from 'react';
import { Users } from 'lucide-react';
import { useRoomStore } from '../stores/roomStore';
import { useInstanceStore } from '../stores/instanceStore';
import { useRoomEvents } from '../hooks/useRoomEvents';
import { RoomSetupView } from '../components/rooms/RoomSetupView';
import { RoomHostView } from '../components/rooms/RoomHostView';
import { RoomGuestView } from '../components/rooms/RoomGuestView';
import { RoomStatusBadge } from '../components/rooms/RoomStatusBadge';
import { Button } from '../components/ui/Button';
import { Input } from '../components/ui/Input';
import { LoadingSpinner } from '../components/ui/LoadingSpinner';
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
    <div className="page animate-fade-in">
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

      {error && (
        <div
          style={{
            background: 'var(--banner-error-bg)',
            border: '1px solid var(--banner-error-border)',
            borderRadius: 'var(--radius-md)',
            padding: 'var(--space-md) var(--space-lg)',
            marginBottom: 'var(--space-xl)',
            color: 'var(--error)',
            fontSize: 'var(--font-size-sm)',
            display: 'flex',
            justifyContent: 'space-between',
            alignItems: 'center',
            gap: 'var(--space-md)',
          }}
        >
          <span>{t('rooms.error_prefix', { error })}</span>
          <button
            onClick={clearError}
            style={{ background: 'none', border: 'none', color: 'var(--error)', cursor: 'pointer', textDecoration: 'underline' }}
          >
            {t('common.dismiss')}
          </button>
        </div>
      )}

      {!loaded && !status ? (
        <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-sm)', color: 'var(--text-secondary)' }}>
          <LoadingSpinner /> {t('rooms.loading')}
        </div>
      ) : !status ? (
        <Button onClick={() => { loadStatus(); }}>{t('rooms.retry')}</Button>
      ) : !tsReady ? (
        <RoomSetupView status={status} progress={progress} busy={busy} onCheck={checkLink} />
      ) : status.role === 'host' ? (
        <RoomHostView status={status} instances={instances} />
      ) : status.role === 'guest' ? (
        <RoomGuestView status={status} instances={instances} />
      ) : (
        <RoomLobby />
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
  const [name, setName] = useState('');
  const [code, setCode] = useState('');

  return (
    <div
      style={{
        display: 'grid',
        gridTemplateColumns: 'repeat(auto-fit, minmax(300px, 1fr))',
        gap: 'var(--space-xl)',
        maxWidth: 860,
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
  );
}

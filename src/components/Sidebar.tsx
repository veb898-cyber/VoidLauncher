import { useAuthStore } from '../stores/authStore';
import { GameRunningBadge } from './launch/GameRunningBadge';
import { Tooltip } from './ui/Tooltip';
import { t } from '../lib/i18n';
import { Avatar } from '../lib/remoteImage';

interface SidebarProps {
  activePage: string;
  onNavigate: (page: string) => void;
}

export function Sidebar({ activePage, onNavigate }: SidebarProps) {
  const { profile, isLoggedIn } = useAuthStore();

  const navItems = [
    { id: 'home', icon: <HomeIcon />, label: t('sidebar.home') },
    { id: 'instances', icon: <InstancesIcon />, label: t('sidebar.instances') },
    { id: 'rooms', icon: <RoomsIcon />, label: t('sidebar.rooms') },
    { id: 'accounts', icon: <AccountsIcon />, label: t('sidebar.accounts') },
    { id: 'terminal', icon: <TerminalIcon />, label: t('sidebar.terminal') },
  ];

  return (
    <nav className="sidebar">
      <div className="sidebar__nav">
        {navItems.map((item) => (
          <Tooltip key={item.id} content={item.label}>
            <button
              className={`sidebar__item ${activePage === item.id ? 'sidebar__item--active' : ''}`}
              onClick={() => onNavigate(item.id)}
            >
              {item.icon}
            </button>
          </Tooltip>
        ))}
      </div>

      <div className="sidebar__bottom">
        <GameRunningBadge />
        <button
          className={`sidebar__item ${activePage === 'settings' ? 'sidebar__item--active' : ''}`}
          onClick={() => onNavigate('settings')}
        >
          <SettingsIcon />
        </button>
        <div className="sidebar__separator" />
        {isLoggedIn && profile ? (
          <div
            className="sidebar__avatar"
            onClick={() => onNavigate('accounts')}
          >
            <Avatar uuid={profile.id} name={profile.name} size={36} />
          </div>
        ) : (
          <Tooltip content={t('sidebar.login')}>
            <button
              className="sidebar__item"
              onClick={() => onNavigate('login')}
            >
              <LoginIcon />
            </button>
          </Tooltip>
        )}
      </div>
    </nav>
  );
}

// Icon components
function HomeIcon() {
  return (
    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <path d="M3 9l9-7 9 7v11a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" />
      <polyline points="9 22 9 12 15 12 15 22" />
    </svg>
  );
}

function InstancesIcon() {
  return (
    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <rect x="3" y="3" width="7" height="7" rx="1" />
      <rect x="14" y="3" width="7" height="7" rx="1" />
      <rect x="3" y="14" width="7" height="7" rx="1" />
      <rect x="14" y="14" width="7" height="7" rx="1" />
    </svg>
  );
}

function RoomsIcon() {
  // "Rooms" = P2P multiplayer: two peer monitors connected by a direct
  // link (both heads are peers, neither is a server) — a small P2P node sits
  // mid-link the way a room's join-token sits between the two players.
  return (
    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      {/* Peer A — верхний-правый монитор */}
      <rect x="13" y="2" width="9" height="7.5" rx="1.2" />
      <path d="M17.5 9.5V12" />
      <path d="M15.25 12h4.5" />

      {/* Peer B — нижний-левый монитор */}
      <rect x="2" y="13.5" width="9" height="7.5" rx="1.2" />
      <path d="M6.5 21V22.5" />
      <path d="M4.25 22.5h4.5" />

      {/* P2P-линк между ближними углами + узел-соединение */}
      <path d="M13 9.5C12 10.8 12 11.8 11 13.2" />
      <circle cx="12" cy="11.3" r="1.1" fill="currentColor" stroke="none" />
    </svg>
  );
}

function AccountsIcon() {
  return (
    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <path d="M17 21v-2a4 4 0 0 0-4-4H5a4 4 0 0 0-4 4v2" />
      <circle cx="9" cy="7" r="4" />
      <path d="M23 21v-2a4 4 0 0 0-3-3.87" />
      <path d="M16 3.13a4 4 0 0 1 0 7.75" />
    </svg>
  );
}

function TerminalIcon() {
  return (
    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <polyline points="4 17 10 11 4 5" />
      <line x1="12" y1="19" x2="20" y2="19" />
    </svg>
  );
}

function SettingsIcon() {
  return (
    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <circle cx="12" cy="12" r="3" />
      <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.68 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 2.83-2.83l.06.06A1.65 1.65 0 0 0 9 4.68a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
    </svg>
  );
}

function LoginIcon() {
  return (
    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <path d="M15 3h4a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2h-4" />
      <polyline points="10 17 15 12 10 7" />
      <line x1="15" y1="12" x2="3" y2="12" />
    </svg>
  );
}

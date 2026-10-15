import { useCallback, useEffect, useLayoutEffect, useRef, useState } from 'react';
import { Liquid } from 'liquid-gooey';
import { useAuthStore } from '../stores/authStore';
import { GameRunningBadge } from './launch/GameRunningBadge';
import { Tooltip } from './ui/Tooltip';
import { t } from '../lib/i18n';
import { Avatar } from '../lib/remoteImage';
import { useGooey } from '../hooks/useGooey';

interface SidebarProps {
  activePage: string;
  onNavigate: (page: string) => void;
}

const ACCENT_HEIGHT = 24;

export function Sidebar({ activePage, onNavigate }: SidebarProps) {
  const { profile, isLoggedIn } = useAuthStore();
  const gooey = useGooey();
  const sidebarRef = useRef<HTMLElement>(null);
  const itemRefs = useRef(new Map<string, HTMLButtonElement>());
  // Null until measured, so the accent mounts already at its place instead
  // of sliding in from the top on first paint.
  const [accent, setAccent] = useState<{ top: number; left: number } | null>(null);

  const navItems = [
    { id: 'home', icon: <HomeIcon />, label: t('sidebar.home') },
    { id: 'instances', icon: <InstancesIcon />, label: t('sidebar.instances') },
    { id: 'rooms', icon: <RoomsIcon />, label: t('sidebar.rooms') },
    { id: 'accounts', icon: <AccountsIcon />, label: t('sidebar.accounts') },
    { id: 'terminal', icon: <TerminalIcon />, label: t('sidebar.terminal') },
  ];

  // Rect-based on purpose: the accent is absolutely positioned in the
  // sidebar's padding box, and the buttons sit in two different containers
  // (nav + bottom) with the bottom one anchored — offsetTop quirks are not
  // worth debugging when two rects give the same numbers directly. The
  // -8px matches the old ::before (item's left edge, 8px outward).
  const measure = useCallback(() => {
    const sidebar = sidebarRef.current;
    const btn = itemRefs.current.get(activePage);
    if (!sidebar || !btn) {
      setAccent(null);
      return;
    }
    const s = sidebar.getBoundingClientRect();
    const b = btn.getBoundingClientRect();
    setAccent({
      top: b.top - s.top + (b.height - ACCENT_HEIGHT) / 2,
      left: b.left - s.left - 8,
    });
  }, [activePage]);

  useLayoutEffect(measure, [measure]);

  // Window resizes change the sidebar's height (it stretches full-height),
  // which can shift the bottom-anchored settings button.
  useEffect(() => {
    const el = sidebarRef.current;
    if (!el) return;
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    return () => ro.disconnect();
  }, [measure]);

  const itemRef = (id: string) => (el: HTMLButtonElement | null) => {
    if (el) itemRefs.current.set(id, el);
    else itemRefs.current.delete(id);
  };

  const accentStyle = accent
    ? { transform: `translate(${accent.left}px, ${accent.top}px)` }
    : undefined;

  return (
    <nav className="sidebar" ref={sidebarRef}>
      {/* The sliding active-item accent (was a per-item ::before that
          teleported). With gooey it rides a liquid group: the blob hides
          under the crisp bar at rest and trails it while it travels.
          The default move tuning reads badly on a thin 3×24 bar: the loose
          trail droplet detaches and the bowed edges split the silhouette
          into stray pieces (owner: «полоска … отделяется/копируется»), so
          the surface follows the bar almost 1:1 with no tail — the gooey
          look stays in the soft blurred edge, not in a runaway blob. */}
      {accent && gooey ? (
        <Liquid
          fill="var(--primary)"
          blur={2.5}
          style={{ position: 'absolute', inset: 0, pointerEvents: 'none' }}
        >
          <Liquid.Item
            effect="move"
            move={{
              springiness: 0.9,
              wobble: 0.1,
              stretch: 0.1,
              trail: 0,
              advanced: { bend: 0, bendX: 0 },
            }}
          >
            <span className="sidebar__accent" style={accentStyle} aria-hidden />
          </Liquid.Item>
        </Liquid>
      ) : (
        accent && <span className="sidebar__accent" style={accentStyle} aria-hidden />
      )}

      <div className="sidebar__nav">
        {navItems.map((item) => (
          <Tooltip key={item.id} content={item.label}>
            <button
              ref={itemRef(item.id)}
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
          ref={itemRef('settings')}
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
            {/* The 2px border eats into the 36px box (border-box), so the head
                must fill the 32px content box — otherwise it renders at 36px
                and gets clipped, which reads as an over-zoomed avatar. */}
            <Avatar
              uuid={profile.id}
              name={profile.name}
              size={36}
              style={{ width: '100%', height: '100%' }}
            />
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

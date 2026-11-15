import { useEffect, useState } from 'react';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { t } from '../lib/i18n';
import slimeIcon from '../assets/slime.png';

export function Titlebar() {
  // `getCurrentWindow()` returns a fresh handle on every call; keep one handle
  // for the component's lifetime so the resize listener is registered once.
  const [appWindow] = useState(getCurrentWindow);
  const [isMaximized, setIsMaximized] = useState(false);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    // Resize events can resolve `isMaximized()` out of order. Only the newest
    // request may update React state.
    let request = 0;

    const refresh = async () => {
      const current = ++request;
      try {
        const maximized = await appWindow.isMaximized();
        if (!disposed && current === request) {
          setIsMaximized(maximized);
        }
      } catch {
        // During window teardown the API may already be unavailable.
      }
    };

    void refresh();
    appWindow
      .onResized(() => {
        void refresh();
      })
      .then((fn) => {
        if (disposed) fn();
        else unlisten = fn;
      })
      .catch(() => {
        // A missing permission must not break the titlebar.
      });

    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [appWindow]);

  return (
    <div className="titlebar">
      <div className="titlebar__logo">
        <img src={slimeIcon} alt="" className="titlebar__logo-icon" draggable={false} />
        {t('titlebar.logo')}
      </div>

      <div className="titlebar__controls">
        <button
          className="titlebar__btn"
          onClick={() => appWindow.minimize()}
          aria-label={t('titlebar.minimize')}
        >
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
            <line x1="5" y1="12" x2="19" y2="12" />
          </svg>
        </button>
        <button
          className="titlebar__btn"
          onClick={() => {
            // toggleMaximize is atomic: two fast clicks cannot both read
            // "not maximized" and both call maximize (the old race).
            appWindow.toggleMaximize().catch(() => {});
          }}
          aria-label={isMaximized ? t('titlebar.restore') : t('titlebar.maximize')}
        >
          {isMaximized ? (
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
              <rect x="8" y="8" width="11" height="11" rx="1" />
              <path d="M5 16V5h11" />
            </svg>
          ) : (
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
              <rect x="5" y="5" width="14" height="14" rx="1" />
            </svg>
          )}
        </button>
        <button
          className="titlebar__btn titlebar__btn--close"
          onClick={() => appWindow.close()}
          aria-label={t('titlebar.close')}
        >
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
            <line x1="6" y1="6" x2="18" y2="18" />
            <line x1="6" y1="18" x2="18" y2="6" />
          </svg>
        </button>
      </div>
    </div>
  );
}

import { Suspense, lazy, useEffect, useState, useCallback } from 'react';
import { listen } from '@tauri-apps/api/event';
import { Titlebar } from './components/Titlebar';
import { Sidebar } from './components/Sidebar';
import LiquidGlassWallpaper from './components/LiquidGlassWallpaper';
import { ToastContainer } from './components/ui/Toast';
import { InstallOverlay } from './components/install/InstallOverlay';
import { UpdaterModal } from './components/UpdaterModal';
import { JavaSetupModal } from './components/launch/JavaSetupModal';
import { ErrorBoundary } from './components/ErrorBoundary';
import { ConfirmDialog } from './components/ui/ConfirmDialog';
import { LoadingSpinner } from './components/ui/LoadingSpinner';
import { useAuthStore } from './stores/authStore';
import { useSettingsStore } from './stores/settingsStore';
import { useAccountsStore } from './stores/accountsStore';
import { useInstanceStore } from './stores/instanceStore';
import { useGameEvents } from './hooks/useGameEvents';
import { useKeyboardShortcuts } from './hooks/useKeyboardShortcuts';
import { useFocusStore } from './stores/focusStore';
import { useUpdater } from './hooks/useUpdater';
import { useBrowserGuardStore } from './stores/browserGuardStore';
import { useJavaDownloadStore } from './stores/javaDownloadStore';
import { useT } from './lib/i18n';

const Home = lazy(() => import('./pages/Home').then(m => ({ default: m.Home })));
const Login = lazy(() => import('./pages/Login').then(m => ({ default: m.Login })));
const Settings = lazy(() => import('./pages/Settings').then(m => ({ default: m.Settings })));
const Terminal = lazy(() => import('./pages/Terminal').then(m => ({ default: m.Terminal })));
const Accounts = lazy(() => import('./pages/Accounts').then(m => ({ default: m.Accounts })));
const Rooms = lazy(() => import('./pages/Rooms').then(m => ({ default: m.Rooms })));
const HomeLayout = lazy(() => import('./components/layout/HomeLayout').then(m => ({ default: m.HomeLayout })));

function App() {
  useGameEvents();
  useKeyboardShortcuts();
  const t = useT();
  const updater = useUpdater();
  const [activePage, setActivePage] = useState('home');
  const { checkAuth } = useAuthStore();
  const { loadConfig } = useSettingsStore();
  const loadAccounts = useAccountsStore((s) => s.loadAccounts);
  const isFrozen = useFocusStore((s) => s.isFrozen);

  const [javaSetupVisible, setJavaSetupVisible] = useState(false);
  const leavePending = useBrowserGuardStore((s) => s.pending);
  const leaveUnsaved = useBrowserGuardStore((s) => s.unsaved);
  const leaveRequest = useBrowserGuardStore((s) => s.request);

  // Sidebar / page navigation is guarded: if the content browser has selected
  // items, switching pages first asks for confirmation (the selection would be lost).
  const navigate = useCallback((page: string) => {
    useBrowserGuardStore.getState().askLeave(() => setActivePage(page));
  }, []);

  // Listen for java_download_progress during launch to show the setup modal
  useEffect(() => {
    const isLaunchingRef = { current: false };
    const unsub = useInstanceStore.subscribe((s) => {
      isLaunchingRef.current = s.isLaunching;
      if (!s.isLaunching) {
        setJavaSetupVisible(false);
      }
    });

    const unlisten = listen<{ major_version: number; stage: string; percent: number; message: string }>(
      'java_download_progress',
      (event) => {
        const p = event.payload;
        if (p.stage === 'done' || p.stage === 'error') {
          // Both are terminal: stop showing progress (the launch modal shows
          // the error text itself when it catches the event).
          useJavaDownloadStore.getState().clear();
          if (p.stage === 'done' && isLaunchingRef.current) {
            setTimeout(() => setJavaSetupVisible(false), 500);
          }
        } else {
          // Keep the global store fresh even while Settings is not mounted,
          // so re-entering it mid-download still shows progress.
          useJavaDownloadStore.getState().reportProgress(p.major_version, p.percent, p.message);
          if (isLaunchingRef.current) {
            setJavaSetupVisible(true);
          }
        }
      },
    );

    return () => { unsub(); unlisten.then((fn) => fn()).catch(() => {}); };
  }, []);

  useEffect(() => {
    checkAuth();
    loadConfig();
    loadAccounts();
    const splash = document.getElementById('splash');
    if (splash) {
      splash.classList.add('fade-out');
      setTimeout(() => splash.remove(), 500);
    }
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Disable browser context menu (right-click)
  useEffect(() => {
    const handler = (e: MouseEvent) => { e.preventDefault(); };
    window.addEventListener('contextmenu', handler);
    return () => window.removeEventListener('contextmenu', handler);
  }, []);

  // Toggle a global class that pauses animations and disables transitions
  // when the launcher is frozen (window unfocused + game running).
  useEffect(() => {
    const root = document.body;
    if (isFrozen) root.classList.add('app-frozen');
    else root.classList.remove('app-frozen');
    return () => { root.classList.remove('app-frozen'); };
  }, [isFrozen]);

  // Mirror the active page for CSS: the Liquid Glass rail-light strip
  // (.lg-substrate::after) is shown only on the instances tab.
  useEffect(() => {
    document.documentElement.dataset.page = activePage;
  }, [activePage]);

  const renderPage = () => {
    switch (activePage) {
      case 'home':
        return <Home onNavigate={navigate} />;
      case 'terminal':
        return <Terminal />;
      case 'login':
        return <Login onNavigate={navigate} />;
      case 'instances':
        return <HomeLayout onNavigate={navigate} />;
      case 'accounts':
        return <Accounts />;
      case 'rooms':
        return <Rooms />;
      case 'settings':
        return <Settings />;
      default:
        return <Home onNavigate={navigate} />;
    }
  };

  return (
    <>
      {/* Substrate for the experimental Liquid Glass appearance: the field the
          glass surfaces actually refract, plus the chosen animated backdrop
          painted into it. Out of flow (position: fixed) and invisible unless
          Liquid Glass is applied, so the normal themes are untouched. */}
      <LiquidGlassWallpaper />
      <Titlebar />
      <div className="app-layout">
        <Sidebar activePage={activePage} onNavigate={navigate} />
        <main className="main-content">
          <ErrorBoundary>
            <Suspense
              fallback={
                <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'center', padding: 'var(--space-3xl)' }}>
                  <LoadingSpinner size={28} />
                </div>
              }
            >
              {renderPage()}
            </Suspense>
          </ErrorBoundary>
        </main>
      </div>
      <InstallOverlay />
      <UpdaterModal
        updateAvailable={updater.updateAvailable}
        updateInfo={updater.updateInfo}
        downloading={updater.downloading}
        downloadProgress={updater.downloadProgress}
        installing={updater.installing}
        error={updater.error}
        checkError={updater.checkError}
        onUpdate={updater.downloadAndInstall}
        onDismiss={updater.dismissUpdate}
        onRetryCheck={updater.retryCheck}
        onDismissCheckError={updater.dismissCheckError}
      />
      <ToastContainer />
      <JavaSetupModal
        open={javaSetupVisible}
        onClose={() => setJavaSetupVisible(false)}
      />

      {/* Leave guard: navigating away would discard the content browser
          selection or unsaved settings changes. Shared confirm modal —
          in the Liquid Glass theme it renders as scrim + glass sheet. */}
      <ConfirmDialog
        open={!!leaveRequest}
        title={leaveUnsaved && leavePending === 0 ? t('content.leave_unsaved_title') : t('content.leave_confirm_title')}
        description={
          leaveUnsaved && leavePending === 0
            ? t('content.leave_unsaved_text')
            : t('content.leave_confirm_text', { count: leavePending.toString() })
        }
        onConfirm={() => useBrowserGuardStore.getState().resolveLeave()}
        onCancel={() => useBrowserGuardStore.getState().cancelLeave()}
        confirmLabel={t('common.leave')}
      />
    </>
  );
}

export default App;

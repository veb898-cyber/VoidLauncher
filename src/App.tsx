import { Suspense, lazy, useEffect, useState, useCallback } from 'react';
import { listen } from '@tauri-apps/api/event';
import { Titlebar } from './components/Titlebar';
import { Sidebar } from './components/Sidebar';
import { ToastContainer } from './components/ui/Toast';
import { InstallOverlay } from './components/install/InstallOverlay';
import { UpdaterModal } from './components/UpdaterModal';
import { LoaderInstallModal } from './components/launch/LoaderInstallModal';
import { JavaSetupModal } from './components/launch/JavaSetupModal';
import { ErrorBoundary } from './components/ErrorBoundary';
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
import { Button } from './components/ui/Button';
import { useT } from './lib/i18n';

const Home = lazy(() => import('./pages/Home').then(m => ({ default: m.Home })));
const Login = lazy(() => import('./pages/Login').then(m => ({ default: m.Login })));
const Settings = lazy(() => import('./pages/Settings').then(m => ({ default: m.Settings })));
const Logs = lazy(() => import('./pages/Logs').then(m => ({ default: m.Logs })));
const GameLogs = lazy(() => import('./pages/GameLogs').then(m => ({ default: m.GameLogs })));
const Accounts = lazy(() => import('./pages/Accounts').then(m => ({ default: m.Accounts })));
const Modpacks = lazy(() => import('./pages/Modpacks').then(m => ({ default: m.Modpacks })));
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
  const pendingLoaderInstall = useInstanceStore((s) => s.pendingLoaderInstall);
  const dismissLoaderInstall = useInstanceStore((s) => s.dismissLoaderInstall);
  const launchGame = useInstanceStore((s) => s.launchGame);

  const handleLoaderInstalled = useCallback(() => {
    if (!pendingLoaderInstall) return;
    const name = pendingLoaderInstall.instanceName;
    dismissLoaderInstall();
    launchGame(name);
  }, [pendingLoaderInstall, dismissLoaderInstall, launchGame]);

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
        if (p.stage === 'done') {
          useJavaDownloadStore.getState().clear();
          if (isLaunchingRef.current) {
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

  const renderPage = () => {
    switch (activePage) {
      case 'home':
        return <Home onNavigate={navigate} />;
      case 'game_logs':
        return <GameLogs />;
      case 'login':
        return <Login onNavigate={navigate} />;
      case 'instances':
        return <HomeLayout onNavigate={navigate} />;
      case 'modpacks':
        return <Modpacks />;
      case 'accounts':
        return <Accounts />;
      case 'logs':
        return <Logs />;
      case 'settings':
        return <Settings />;
      default:
        return <Home onNavigate={navigate} />;
    }
  };

  return (
    <>
      <Titlebar />
      <div className="app-layout">
        <Sidebar activePage={activePage} onNavigate={navigate} />
        <main className="main-content">
          <ErrorBoundary>
            <Suspense fallback={<div />}>
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
      {pendingLoaderInstall && (
        <LoaderInstallModal
          open={true}
          onClose={dismissLoaderInstall}
          onInstalled={handleLoaderInstalled}
          instanceName={pendingLoaderInstall.instanceName}
        />
      )}
      <JavaSetupModal
        open={javaSetupVisible}
        onClose={() => setJavaSetupVisible(false)}
      />

      {/* Leave guard: navigating away would discard the content browser
          selection or unsaved settings changes. */}
      {leaveRequest && (
        <div style={{ position: 'fixed', inset: 0, background: 'rgba(0,0,0,0.6)', display: 'flex', alignItems: 'center', justifyContent: 'center', zIndex: 9999 }}>
          <div className="glass-card" style={{ padding: 'var(--space-xl)', maxWidth: 400, width: '90%' }}>
            {leaveUnsaved && leavePending === 0 ? (
              <>
                <h3 style={{ margin: '0 0 var(--space-md)' }}>{t('content.leave_unsaved_title')}</h3>
                <p style={{ color: 'var(--text-secondary)', marginBottom: 'var(--space-lg)' }}>{t('content.leave_unsaved_text')}</p>
              </>
            ) : (
              <>
                <h3 style={{ margin: '0 0 var(--space-md)' }}>{t('content.leave_confirm_title')}</h3>
                <p style={{ color: 'var(--text-secondary)', marginBottom: 'var(--space-lg)' }}>{t('content.leave_confirm_text', { count: leavePending.toString() })}</p>
              </>
            )}
            <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 'var(--space-sm)' }}>
              <Button variant="ghost" onClick={() => useBrowserGuardStore.getState().cancelLeave()}>{t('common.cancel')}</Button>
              <Button onClick={() => useBrowserGuardStore.getState().resolveLeave()}>{t('common.leave')}</Button>
            </div>
          </div>
        </div>
      )}
    </>
  );
}

export default App;

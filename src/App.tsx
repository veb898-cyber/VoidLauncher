import { Suspense, lazy, useEffect, useState, useCallback } from 'react';
import { Titlebar } from './components/Titlebar';
import { Sidebar } from './components/Sidebar';
import { ToastContainer } from './components/ui/Toast';
import { InstallOverlay } from './components/install/InstallOverlay';
import { UpdaterModal } from './components/UpdaterModal';
import { LoaderInstallModal } from './components/launch/LoaderInstallModal';
import { ErrorBoundary } from './components/ErrorBoundary';
import { useAuthStore } from './stores/authStore';
import { useSettingsStore } from './stores/settingsStore';
import { useAccountsStore } from './stores/accountsStore';
import { useInstanceStore } from './stores/instanceStore';
import { useGameEvents } from './hooks/useGameEvents';
import { useKeyboardShortcuts } from './hooks/useKeyboardShortcuts';
import { useFocusStore } from './stores/focusStore';
import { useUpdater } from './hooks/useUpdater';

const Home = lazy(() => import('./pages/Home').then(m => ({ default: m.Home })));
const Login = lazy(() => import('./pages/Login').then(m => ({ default: m.Login })));
const Settings = lazy(() => import('./pages/Settings').then(m => ({ default: m.Settings })));
const Logs = lazy(() => import('./pages/Logs').then(m => ({ default: m.Logs })));
const GameLogs = lazy(() => import('./pages/GameLogs').then(m => ({ default: m.GameLogs })));
const Accounts = lazy(() => import('./pages/Accounts').then(m => ({ default: m.Accounts })));
const HomeLayout = lazy(() => import('./components/layout/HomeLayout').then(m => ({ default: m.HomeLayout })));

function App() {
  useGameEvents();
  useKeyboardShortcuts();
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
        return <Home onNavigate={setActivePage} />;
      case 'game_logs':
        return <GameLogs />;
      case 'login':
        return <Login onNavigate={setActivePage} />;
      case 'instances':
        return <HomeLayout onNavigate={setActivePage} />;
      case 'accounts':
        return <Accounts />;
      case 'logs':
        return <Logs />;
      case 'settings':
        return <Settings />;
      default:
        return <Home onNavigate={setActivePage} />;
    }
  };

  return (
    <>
      <Titlebar />
      <div className="app-layout">
        <Sidebar activePage={activePage} onNavigate={setActivePage} />
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
        onUpdate={updater.downloadAndInstall}
        onDismiss={updater.dismissUpdate}
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
    </>
  );
}

export default App;

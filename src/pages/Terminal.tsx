import { useEffect, useState } from 'react';
import { useT } from '../lib/i18n';
import { LauncherLogs } from './Logs';
import { GameLogs } from './GameLogs';
import { SlidingTabs } from '../components/ui/SlidingTabs';

type TerminalView = 'launcher' | 'game';

// "Терминал": one page for both log sources. The launcher's own activity
// ("Логи лаунчера") and the live unified Minecraft console ("Игровые логи")
// are two tabs of the same screen.
export function Terminal() {
  const t = useT();
  // Remember the last opened tab so leaving and returning to the page does
  // not silently switch the user back to the other log source.
  const [view, setView] = useState<TerminalView>(() =>
    sessionStorage.getItem('terminal.view') === 'launcher' ? 'launcher' : 'game'
  );

  useEffect(() => {
    sessionStorage.setItem('terminal.view', view);
  }, [view]);

  return (
    <div className="page" style={{ display: 'flex', flexDirection: 'column', height: '100%' }}>
      <div className="page__header" style={{ marginBottom: 'var(--space-md)', flexShrink: 0 }}>
        <h1 className="page__title">{t('terminal.title')}</h1>
        <p className="page__subtitle">{t('terminal.subtitle')}</p>
      </div>

      <SlidingTabs<TerminalView>
        tabs={[
          { id: 'game', label: t('terminal.tab_game') },
          { id: 'launcher', label: t('terminal.tab_launcher') },
        ]}
        active={view}
        onChange={setView}
        ariaLabel={t('terminal.title')}
        style={{ marginBottom: 'var(--space-md)', flexShrink: 0 }}
      />

      {/* Both panes stay mounted: switching tabs must not interrupt the live
          game-log stream, lose its scroll position or reset the launcher log
          filters. The inactive pane is only hidden. */}
      <div style={{ flex: 1, minHeight: 0, display: view === 'game' ? 'flex' : 'none', flexDirection: 'column' }}>
        <GameLogs />
      </div>
      <div style={{ flex: 1, minHeight: 0, display: view === 'launcher' ? 'flex' : 'none', flexDirection: 'column' }}>
        <LauncherLogs />
      </div>
    </div>
  );
}
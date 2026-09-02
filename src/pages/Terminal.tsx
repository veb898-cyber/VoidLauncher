import { useState } from 'react';
import { useT } from '../lib/i18n';
import { LauncherLogs } from './Logs';
import { GameLogs } from './GameLogs';

type TerminalView = 'launcher' | 'game';

// "Терминал": one page for both log sources. The launcher's own activity
// ("Логи лаунчера") and the live unified Minecraft console ("Игровые логи")
// are two tabs of the same screen.
export function Terminal() {
  const t = useT();
  const [view, setView] = useState<TerminalView>('game');

  return (
    <div className="page animate-fade-in" style={{ display: 'flex', flexDirection: 'column', height: '100%' }}>
      <div className="page__header" style={{ marginBottom: 'var(--space-md)', flexShrink: 0 }}>
        <h1 className="page__title">{t('terminal.title')}</h1>
        <p className="page__subtitle">{t('terminal.subtitle')}</p>
      </div>

      <div className="tabs" style={{ marginBottom: 'var(--space-md)', flexShrink: 0 }}>
        <button className={`tab ${view === 'game' ? 'tab--active' : ''}`} onClick={() => setView('game')}>
          {t('terminal.tab_game')}
        </button>
        <button className={`tab ${view === 'launcher' ? 'tab--active' : ''}`} onClick={() => setView('launcher')}>
          {t('terminal.tab_launcher')}
        </button>
      </div>

      <div style={{ flex: 1, minHeight: 0 }}>
        {view === 'launcher' ? <LauncherLogs /> : <GameLogs />}
      </div>
    </div>
  );
}
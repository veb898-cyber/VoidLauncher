import { useState } from 'react';
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
  const [view, setView] = useState<TerminalView>('game');

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

      <div style={{ flex: 1, minHeight: 0 }}>
        {view === 'launcher' ? <LauncherLogs /> : <GameLogs />}
      </div>
    </div>
  );
}
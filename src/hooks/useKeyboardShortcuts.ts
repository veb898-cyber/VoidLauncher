import { useEffect } from 'react';
import { useInstanceStore } from '../stores/instanceStore';

export function useKeyboardShortcuts() {
  const { selectedInstance, launchGame, isLaunching } = useInstanceStore();

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      // Enter to play selected instance
      if (e.key === 'Enter' && selectedInstance && !isLaunching) {
        // Don't trigger if we're in an input
        const tag = (e.target as HTMLElement).tagName;
        if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT') return;
        launchGame(selectedInstance);
      }
    };

    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  }, [selectedInstance, isLaunching, launchGame]);
}

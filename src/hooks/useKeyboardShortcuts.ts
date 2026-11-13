import { useEffect } from 'react';
import { useInstanceStore } from '../stores/instanceStore';

export function useKeyboardShortcuts() {
  // Slice subscriptions: the shortcut only depends on the selection and the
  // launch flag. A whole-store subscription re-rendered App whenever the
  // instance list or any other instance state changed.
  const selectedInstance = useInstanceStore((s) => s.selectedInstance);
  const launchGame = useInstanceStore((s) => s.launchGame);
  const isLaunching = useInstanceStore((s) => s.isLaunching);

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

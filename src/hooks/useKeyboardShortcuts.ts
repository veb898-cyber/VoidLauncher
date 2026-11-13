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
      // Enter to play selected instance.
      if (e.key !== 'Enter' || !selectedInstance || isLaunching) return;
      if (e.ctrlKey || e.metaKey || e.altKey) return;
      const target = e.target;
      if (!(target instanceof Element)) return;
      // Never hijack Enter from a control, an editable field, or a dialog:
      // pressing Enter on a focused "Delete" button already activates that
      // button, and doing both would launch the game as a side effect.
      if (
        target.closest(
          'input, textarea, select, button, a, summary, [role="button"], [role="dialog"], [contenteditable="true"]',
        )
      ) {
        return;
      }
      launchGame(selectedInstance);
    };

    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  }, [selectedInstance, isLaunching, launchGame]);
}

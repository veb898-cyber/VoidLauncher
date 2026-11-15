import { useEffect, useRef, useState } from 'react';

/** Keep a refresh icon visibly spinning even when the operation is fast. */
export const MIN_SPIN_VISIBLE_MS = 600;

/**
 * `active` while the operation runs, plus a short minimum visible window
 * afterwards. The operation itself is never delayed: only the icon class
 * stays on for at most `minimumMs` longer than the real work.
 */
export function useMinimumSpin(active: boolean, minimumMs = MIN_SPIN_VISIBLE_MS): boolean {
  const [visible, setVisible] = useState(active);
  const startedAt = useRef(0);

  useEffect(() => {
    if (active) {
      startedAt.current = Date.now();
      setVisible(true);
      return;
    }
    if (!visible) return;

    const elapsed = Date.now() - startedAt.current;
    const remaining = Math.max(0, minimumMs - elapsed);
    if (remaining === 0) {
      setVisible(false);
      return;
    }
    const timer = window.setTimeout(() => setVisible(false), remaining);
    return () => window.clearTimeout(timer);
  }, [active, minimumMs, visible]);

  return active || visible;
}

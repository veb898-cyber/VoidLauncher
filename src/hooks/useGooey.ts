import { useEffect, useState } from 'react';
import { useThemeStore } from '../stores/themeStore';

/**
 * Gooey setting + OS reduced-motion preference: the user's toggle decides,
 * but prefers-reduced-motion always wins — the liquid trails are real
 * animation, not a curve swap CSS could neutralise.
 */
export function useGooey(): boolean {
  const gooey = useThemeStore((s) => s.gooey);
  const [reduced, setReduced] = useState(() =>
    typeof window !== 'undefined' &&
    window.matchMedia('(prefers-reduced-motion: reduce)').matches,
  );

  useEffect(() => {
    const mq = window.matchMedia('(prefers-reduced-motion: reduce)');
    const onChange = () => setReduced(mq.matches);
    mq.addEventListener('change', onChange);
    return () => mq.removeEventListener('change', onChange);
  }, []);

  return gooey && !reduced;
}

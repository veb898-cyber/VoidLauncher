import { useEffect, useState } from 'react';

/**
 * Gooey trails are always on — they are no longer a user setting. The OS
 * reduced-motion preference still wins: the liquid trails are real animation,
 * not a curve swap CSS could neutralise.
 */
export function useGooey(): boolean {
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

  return !reduced;
}

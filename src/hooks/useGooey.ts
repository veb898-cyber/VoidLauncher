import { useEffect, useState } from 'react';
import { usePerformanceStore } from '../stores/performanceStore';

/**
 * Gooey trails are a visual effect, not a functional requirement. The OS
 * reduced-motion preference always wins; the optimization switch also turns
 * them off in every theme, including Liquid Glass. The crisp fallback stays
 * fully functional.
 */
export function useGooey(): boolean {
  const [reduced, setReduced] = useState(() =>
    typeof window !== 'undefined' &&
    window.matchMedia('(prefers-reduced-motion: reduce)').matches,
  );
  const reduceEffects = usePerformanceStore((s) => s.reduceInterfaceEffects);

  useEffect(() => {
    const mq = window.matchMedia('(prefers-reduced-motion: reduce)');
    const onChange = () => setReduced(mq.matches);
    mq.addEventListener('change', onChange);
    return () => mq.removeEventListener('change', onChange);
  }, []);

  return !reduced && !reduceEffects;
}

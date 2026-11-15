import { RefreshCw } from 'lucide-react';
import type { CSSProperties } from 'react';
import { useMinimumSpin } from '../../hooks/useMinimumSpin';

interface RefreshIconProps {
  /** Real operation state; the icon keeps a short minimum visible spin. */
  spinning: boolean;
  size?: number;
  /** Minimum visible spin window in ms (default: shared 600 ms). */
  minimumMs?: number;
  className?: string;
  style?: CSSProperties;
}

/** Shared refresh/check-update icon with a visible minimum feedback window. */
export function RefreshIcon({
  spinning,
  size = 14,
  minimumMs,
  className = '',
  style,
}: RefreshIconProps) {
  const visible = useMinimumSpin(spinning, minimumMs);
  return (
    <RefreshCw
      size={size}
      className={`${className}${visible ? ' icon-spin' : ''}`.trim()}
      style={style}
      aria-hidden="true"
    />
  );
}

import { Skeleton } from './Skeleton';

interface ResultListSkeletonProps {
  /** "card" mirrors the modpack-card layout, "row" mirrors browser result rows. */
  variant?: 'card' | 'row';
  rows?: number;
}

/**
 * Shimmer placeholders that mirror the shape of real result cards so the
 * loaded content doesn't reflow the layout when it appears.
 */
export function ResultListSkeleton({ variant = 'card', rows = 6 }: ResultListSkeletonProps) {
  return (
    <div role="status" aria-label="Loading">
      {Array.from({ length: rows }).map((_, i) =>
        variant === 'card' ? (
          <div key={i} className="modpack-card modpack-card--skeleton">
            <Skeleton width={44} height={44} borderRadius="8px" style={{ flexShrink: 0 }} />
            <div style={{ flex: 1, minWidth: 0 }}>
              <Skeleton width="55%" height={12} />
              <Skeleton width="85%" height={10} style={{ marginTop: 6 }} />
              <Skeleton width="35%" height={10} style={{ marginTop: 6 }} />
            </div>
          </div>
        ) : (
          <div key={i} style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-sm)', padding: '8px 10px' }}>
            <Skeleton width={36} height={36} borderRadius="6px" style={{ flexShrink: 0 }} />
            <div style={{ flex: 1, minWidth: 0 }}>
              <Skeleton width={160} height={11} style={{ marginBottom: 6 }} />
              <Skeleton width={220} height={10} />
            </div>
          </div>
        ),
      )}
    </div>
  );
}
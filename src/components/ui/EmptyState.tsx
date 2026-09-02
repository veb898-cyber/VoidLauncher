import type { CSSProperties, ReactNode } from 'react';

interface EmptyStateProps {
  icon?: ReactNode;
  title: string;
  description?: string;
  compact?: boolean;
  style?: CSSProperties;
}

/** Calm, themed empty state: optional icon above a title and description. */
export function EmptyState({ icon, title, description, compact = false, style }: EmptyStateProps) {
  return (
    <div className={`empty-state${compact ? ' empty-state--compact' : ''}`} style={style}>
      {icon && <div className="empty-state__icon">{icon}</div>}
      <div className="empty-state__title">{title}</div>
      {description && <div className="empty-state__desc">{description}</div>}
    </div>
  );
}
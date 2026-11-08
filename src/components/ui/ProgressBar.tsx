interface ProgressBarProps {
  percent: number;
  className?: string;
  showLabel?: boolean;
  /** Unknown total: show a moving activity bar instead of a fake percent. */
  indeterminate?: boolean;
}

export function ProgressBar({ percent, className = '', showLabel = false, indeterminate = false }: ProgressBarProps) {
  const clamped = Math.max(0, Math.min(100, percent));

  return (
    <div className={`progress${indeterminate ? ' progress--indeterminate' : ''} ${className}`.trim()}>
      <div className="progress__bar progress-fill" style={indeterminate ? undefined : { width: `${clamped}%` }} />
      {showLabel && !indeterminate && (
        <span style={{
          fontSize: 'var(--font-size-xs)',
          color: 'var(--text-secondary)',
          marginTop: 'var(--space-xs)',
          display: 'block',
          textAlign: 'right',
        }}>
          {Math.round(clamped)}%
        </span>
      )}
    </div>
  );
}

interface ProgressBarProps {
  percent: number;
  className?: string;
  showLabel?: boolean;
}

export function ProgressBar({ percent, className = '', showLabel = false }: ProgressBarProps) {
  const clamped = Math.max(0, Math.min(100, percent));

  return (
    <div className={`progress ${className}`.trim()}>
      <div className="progress__bar" style={{ width: `${clamped}%` }} />
      {showLabel && (
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

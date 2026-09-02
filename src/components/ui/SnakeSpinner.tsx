interface SnakeSpinnerProps {
  size?: number;
  className?: string;
}

/**
 * CSS-only "snake" spinner: a ring with a wide gap whose faint tail
 * brightens into a solid arc with a bright dot running along its edge
 * (Telegram-style sweep). Used for larger loading states; small icon-only
 * buttons keep the regular LoadingSpinner to stay visually light.
 */
export function SnakeSpinner({ size = 20, className = '' }: SnakeSpinnerProps) {
  return (
    <span
      className={`spinner-snake ${className}`.trim()}
      style={{ width: size, height: size }}
      role="status"
      aria-label="Loading"
    >
      <span className="spinner-snake__rot">
        <span className="spinner-snake__track" />
        <span className="spinner-snake__arc" />
        <span className="spinner-snake__dot" />
      </span>
    </span>
  );
}
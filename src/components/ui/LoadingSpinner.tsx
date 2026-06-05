import { Loader2 } from 'lucide-react';

interface LoadingSpinnerProps {
  size?: number;
  className?: string;
}

export function LoadingSpinner({ size = 16, className = '' }: LoadingSpinnerProps) {
  return (
    <Loader2
      size={size}
      className={className}
      style={{ animation: 'spin 1s linear infinite' }}
    />
  );
}

import { Component, type ReactNode, type ErrorInfo } from 'react';
import { AlertTriangle, RefreshCw } from 'lucide-react';
import { Button } from './ui/Button';
import { t } from '../lib/i18n';

interface Props {
  children: ReactNode;
}

interface State {
  hasError: boolean;
  error: Error | null;
}

export class ErrorBoundary extends Component<Props, State> {
  constructor(props: Props) {
    super(props);
    this.state = { hasError: false, error: null };
  }

  static getDerivedStateFromError(error: Error): State {
    return { hasError: true, error };
  }

  componentDidCatch(error: Error, errorInfo: ErrorInfo) {
    console.error('ErrorBoundary caught:', error, errorInfo);
  }

  render() {
    if (this.state.hasError) {
      return (
        <div
          style={{
            display: 'flex',
            flexDirection: 'column',
            alignItems: 'center',
            justifyContent: 'center',
            height: '100%',
            padding: 'var(--space-3xl)',
            textAlign: 'center',
            gap: 'var(--space-lg)',
          }}
        >
          <AlertTriangle size={48} color="var(--error)" />
          <h2 style={{ fontSize: 'var(--font-size-xl)', fontWeight: 600 }}>
            {t('error_boundary.heading')}
          </h2>
          <p style={{ color: 'var(--text-secondary)', maxWidth: 400 }}>
            {this.state.error?.message || t('error_boundary.message')}
          </p>
          <Button onClick={() => this.setState({ hasError: false, error: null })}>
            <RefreshCw size={16} /> {t('error_boundary.try_again')}
          </Button>
        </div>
      );
    }

    return this.props.children;
  }
}

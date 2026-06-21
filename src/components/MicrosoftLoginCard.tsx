import { useEffect, useRef, useState } from 'react';
import { LogIn, Copy, Check, ExternalLink } from 'lucide-react';
import { openUrl } from '@tauri-apps/plugin-opener';
import { useAuthStore } from '../stores/authStore';
import { t } from '../lib/i18n';
import { useLogPlaque } from '../lib/uiLog';
import { LoadingSpinner } from './ui/LoadingSpinner';
import { Button } from './ui/Button';

interface MicrosoftLoginCardProps {
  /**
   * Called when Microsoft login completes successfully
   * (isLoggedIn transitions to true).
   * The Login page uses this to navigate home;
   * the Accounts modal uses this to close itself.
   */
  onSuccess?: () => void;
}

/**
 * The Microsoft OAuth login card. Renders either the "Sign in with
 * Microsoft" button or, once the device-code flow has started, the
 * big copyable user code + "Open browser" button + waiting spinner.
 *
 * Owns its own poll loop (setInterval → pollLogin) so the same
 * card can be mounted from any page or modal without coordinating
 * timers across callers.
 */
export function MicrosoftLoginCard({ onSuccess }: MicrosoftLoginCardProps) {
  const {
    isLoggedIn,
    isLoading,
    error,
    userCode,
    verificationUri,
    startLogin,
    pollLogin,
    clearError,
  } = useAuthStore();
  const pollInterval = useRef<number | null>(null);
  const [copied, setCopied] = useState(false);

  useLogPlaque(error, 'error', 'auth');

  useEffect(() => {
    if (isLoggedIn) {
      onSuccess?.();
    }
  }, [isLoggedIn, onSuccess]);

  useEffect(() => {
    if (userCode && !isLoggedIn) {
      pollInterval.current = window.setInterval(() => {
        pollLogin();
      }, 5000);
    }

    return () => {
      if (pollInterval.current) {
        clearInterval(pollInterval.current);
        pollInterval.current = null;
      }
    };
  }, [userCode, isLoggedIn, pollLogin]);

  const handleCopyCode = () => {
    if (userCode) {
      navigator.clipboard.writeText(userCode);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    }
  };

  const handleOpenLink = async () => {
    if (verificationUri) {
      await openUrl(verificationUri);
    }
  };

  return (
    <div className="glass-card login-card">
      <div className="login-card__icon">
        <LogIn size={40} color="white" />
      </div>

      <h1 className="login-card__title">{t('login.title')}</h1>
      <p className="login-card__desc">
        {t('login.desc')}
      </p>

      {error && (
        <div style={{
          background: 'var(--banner-error-bg)',
          border: '1px solid var(--banner-error-border)',
          borderRadius: 'var(--radius-md)',
          padding: 'var(--space-md)',
          marginBottom: 'var(--space-lg)',
          color: 'var(--error)',
          fontSize: 'var(--font-size-sm)',
        }}>
          {error}
          <button
            onClick={clearError}
            style={{
              marginLeft: 'var(--space-sm)',
              background: 'none',
              border: 'none',
              color: 'var(--error)',
              cursor: 'pointer',
              textDecoration: 'underline',
            }}
          >
            {t('common.dismiss')}
          </button>
        </div>
      )}

      {!userCode ? (
        <Button
          size="lg"
          onClick={startLogin}
          disabled={isLoading}
          style={{ width: '100%' }}
          loading={isLoading}
          id="login-button"
        >
          <MicrosoftIcon /> {t('login.sign_in_btn')}
        </Button>
      ) : (
        <div className="animate-slide-up">
          <div style={{
            background: 'var(--surface-glass)',
            border: '1px solid var(--surface-border)',
            borderRadius: 'var(--radius-lg)',
            padding: 'var(--space-xl)',
            marginBottom: 'var(--space-lg)',
          }}>
            <p style={{
              fontSize: 'var(--font-size-sm)',
              color: 'var(--text-secondary)',
              marginBottom: 'var(--space-md)',
            }}>
              {t('login.instruction')}
            </p>

            <div
              onClick={handleCopyCode}
              style={{
                fontSize: 'var(--font-size-3xl)',
                fontWeight: 800,
                letterSpacing: '4px',
                fontFamily: 'monospace',
                background: 'var(--gradient-primary)',
                WebkitBackgroundClip: 'text',
                WebkitTextFillColor: 'transparent',
                cursor: 'pointer',
                marginBottom: 'var(--space-sm)',
                display: 'flex',
                alignItems: 'center',
                justifyContent: 'center',
                gap: 'var(--space-sm)',
              }}
              title={t('login.click_to_copy')}
            >
              {userCode}
              {copied ? <Check size={24} color="var(--success)" /> : <Copy size={24} />}
            </div>

            <p style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-tertiary)' }}>
              {copied ? t('login.copied') : t('login.click_to_copy')}
            </p>
          </div>

          <Button
            variant="secondary"
            size="lg"
            onClick={handleOpenLink}
            style={{ width: '100%', marginBottom: 'var(--space-md)' }}
          >
            <ExternalLink size={16} /> {t('login.open_browser')}
          </Button>

          <div style={{
            display: 'flex',
            alignItems: 'center',
            justifyContent: 'center',
            gap: 'var(--space-sm)',
            color: 'var(--text-tertiary)',
            fontSize: 'var(--font-size-sm)',
          }}>
            <LoadingSpinner />
            {t('login.waiting')}
          </div>
        </div>
      )}
    </div>
  );
}

function MicrosoftIcon() {
  return (
    <svg width="18" height="18" viewBox="0 0 21 21" fill="none">
      <rect x="1" y="1" width="9" height="9" fill="#F25022" />
      <rect x="11" y="1" width="9" height="9" fill="#7FBA00" />
      <rect x="1" y="11" width="9" height="9" fill="#00A4EF" />
      <rect x="11" y="11" width="9" height="9" fill="#FFB900" />
    </svg>
  );
}

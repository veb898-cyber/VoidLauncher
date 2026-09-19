import { ShieldCheck, ExternalLink, CheckCircle2, Download } from 'lucide-react';
import { openUrl } from '@tauri-apps/plugin-opener';
import { Button } from '../ui/Button';
import { ProgressBar } from '../ui/ProgressBar';
import { LoadingSpinner } from '../ui/LoadingSpinner';
import type { RoomStatus, RoomProgress } from '../../stores/roomStore';
import { useT } from '../../lib/i18n';

interface RoomSetupViewProps {
  status: RoomStatus;
  progress: RoomProgress | null;
  busy: boolean;
  onCheck: () => void;
}

/**
 * First-run setup: install and sign in to Tailscale. The launcher never asks
 * for credentials — the sign-in link opens in the system browser and the
 * launcher polls the connection state until it turns ready.
 */
export function RoomSetupView({ status, progress, busy, onCheck }: RoomSetupViewProps) {
  const t = useT();
  const ts = status.tailscale;
  const waitingLogin = ts.installed && !ts.logged_in;

  return (
    <div className="glass-card animate-slide-up" style={{ maxWidth: 720 }}>
      <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-md)', marginBottom: 'var(--space-lg)' }}>
        <ShieldCheck size={28} color="var(--primary)" />
        <h2 style={{ margin: 0, fontSize: 'var(--font-size-xl)' }}>{t('rooms.setup_title')}</h2>
      </div>

      <p style={{ color: 'var(--text-secondary)', lineHeight: 1.6, marginBottom: 'var(--space-xl)' }}>
        {t('rooms.setup_desc')}
      </p>

      <div
        style={{
          background: 'var(--surface-glass)',
          border: '1px solid var(--surface-border)',
          borderRadius: 'var(--radius-md)',
          padding: 'var(--space-lg)',
          marginBottom: 'var(--space-xl)',
          display: 'flex',
          alignItems: 'flex-start',
          gap: 'var(--space-md)',
        }}
      >
        {ts.installed && ts.logged_in ? (
          <CheckCircle2 size={22} color="var(--success)" style={{ flexShrink: 0, marginTop: 2 }} />
        ) : (
          <Download size={22} color="var(--primary)" style={{ flexShrink: 0, marginTop: 2 }} />
        )}
        <div style={{ flex: 1 }}>
          {!ts.installed && (
            <p style={{ margin: 0 }}>{t('rooms.setup_not_installed')}</p>
          )}
          {ts.installed && !ts.logged_in && (
            <>
              <p style={{ margin: 0 }}>
                {t('rooms.setup_not_logged_in')}
              </p>
              {ts.version && (
                <p style={{ margin: 'var(--space-xs) 0 0', fontSize: 'var(--font-size-sm)', color: 'var(--text-tertiary)' }}>
                  {t('rooms.setup_installed', { version: ts.version })}
                </p>
              )}
            </>
          )}
          {ts.installed && ts.logged_in && (
            <p style={{ margin: 0 }}>
              {ts.login_name ? t('rooms.setup_signed_in', { name: ts.login_name }) : t('rooms.setup_ready')}
            </p>
          )}
        </div>
      </div>

      {progress && (
        <div style={{ marginBottom: 'var(--space-xl)' }}>
          <div style={{ display: 'flex', justifyContent: 'space-between', marginBottom: 'var(--space-sm)' }}>
            <span style={{ fontSize: 'var(--font-size-sm)', color: 'var(--text-secondary)' }}>{progress.message}</span>
          </div>
          <ProgressBar percent={progress.fraction * 100} />
        </div>
      )}

      <div style={{ display: 'flex', flexWrap: 'wrap', gap: 'var(--space-md)', alignItems: 'center' }}>
        <Button onClick={onCheck} loading={busy} disabled={busy}>
          {busy ? t('rooms.setup_checking') : t('rooms.setup_check')}
        </Button>

        {waitingLogin && ts.auth_url && (
          <Button variant="secondary" onClick={() => { openUrl(ts.auth_url!); }}>
            <ExternalLink size={16} style={{ marginRight: 6 }} />
            {t('rooms.setup_open_login')}
          </Button>
        )}

        {waitingLogin && (
          <span style={{ display: 'inline-flex', alignItems: 'center', gap: 'var(--space-sm)', color: 'var(--text-tertiary)', fontSize: 'var(--font-size-sm)' }}>
            <LoadingSpinner size={14} />
            {t('rooms.setup_waiting')}
          </span>
        )}
      </div>

      <p style={{ marginTop: 'var(--space-xl)', marginBottom: 0, fontSize: 'var(--font-size-sm)', color: 'var(--text-tertiary)', lineHeight: 1.5 }}>
        {t('rooms.setup_secure')}
      </p>
    </div>
  );
}

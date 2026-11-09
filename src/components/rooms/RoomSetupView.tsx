import { ShieldCheck, ExternalLink, Download, Network } from 'lucide-react';
import { openUrl } from '@tauri-apps/plugin-opener';
import { Button } from '../ui/Button';
import { ProgressBar } from '../ui/ProgressBar';
import { LoadingSpinner } from '../ui/LoadingSpinner';
import { RefreshIcon } from '../ui/RefreshIcon';
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
  const steps = [
    { title: t('rooms.setup_step1_title'), desc: t('rooms.setup_step1_desc') },
    { title: t('rooms.setup_step2_title'), desc: t('rooms.setup_step2_desc') },
    { title: t('rooms.setup_step3_title'), desc: t('rooms.setup_step3_desc') },
  ];

  return (
    <div
      className="glass-card animate-slide-up"
      style={{ maxWidth: 760, width: '100%', marginInline: 'auto' }}
    >
      <div
        style={{
          display: 'flex',
          alignItems: 'flex-start',
          gap: 'var(--space-lg)',
          marginBottom: 'var(--space-xl)',
        }}
      >
        <div
          aria-hidden
          style={{
            width: 56,
            height: 56,
            borderRadius: 'var(--radius-full)',
            background: 'var(--primary-dim)',
            color: 'var(--primary)',
            display: 'grid',
            placeItems: 'center',
            flexShrink: 0,
          }}
        >
          <Network size={28} />
        </div>
        <div style={{ minWidth: 0 }}>
          <h2 style={{ margin: '0 0 var(--space-sm)', fontSize: 'var(--font-size-xl)' }}>
            {t('rooms.setup_title')}
          </h2>
          <p style={{ margin: 0, color: 'var(--text-secondary)', lineHeight: 1.6 }}>
            {t('rooms.setup_why_desc')}
          </p>
        </div>
      </div>

      <div style={{ marginBottom: 'var(--space-xl)' }}>
        <div style={{ fontSize: 'var(--font-size-sm)', fontWeight: 600, marginBottom: 'var(--space-md)' }}>
          {t('rooms.setup_steps_title')}
        </div>
        <div
          style={{
            display: 'grid',
            gridTemplateColumns: 'repeat(auto-fit, minmax(190px, 1fr))',
            gap: 'var(--space-md)',
          }}
        >
          {steps.map((step, index) => (
            <div key={step.title} style={{ display: 'flex', gap: 'var(--space-sm)', minWidth: 0 }}>
              <span
                aria-hidden
                style={{
                  flexShrink: 0,
                  width: 24,
                  height: 24,
                  borderRadius: 'var(--radius-full)',
                  background: 'var(--primary-dim)',
                  color: 'var(--primary)',
                  fontSize: 'var(--font-size-xs)',
                  fontWeight: 700,
                  display: 'inline-flex',
                  alignItems: 'center',
                  justifyContent: 'center',
                }}
              >
                {index + 1}
              </span>
              <div style={{ minWidth: 0 }}>
                <div style={{ fontSize: 'var(--font-size-sm)', fontWeight: 600, marginBottom: 2 }}>
                  {step.title}
                </div>
                <div style={{ fontSize: 'var(--font-size-xs)', color: 'var(--text-secondary)', lineHeight: 1.5 }}>
                  {step.desc}
                </div>
              </div>
            </div>
          ))}
        </div>
      </div>

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
        {ts.installed ? (
          <ShieldCheck size={22} color="var(--primary)" style={{ flexShrink: 0, marginTop: 2 }} />
        ) : (
          <Download size={22} color="var(--primary)" style={{ flexShrink: 0, marginTop: 2 }} />
        )}
        <div style={{ flex: 1, minWidth: 0 }}>
          {!ts.installed && (
            <>
              <div style={{ fontWeight: 600, marginBottom: 2 }}>
                {t('rooms.setup_not_installed_title')}
              </div>
              <p style={{ margin: 0, color: 'var(--text-secondary)' }}>
                {t('rooms.setup_not_installed')}
              </p>
            </>
          )}
          {ts.installed && !ts.logged_in && (
            <>
              <div style={{ fontWeight: 600, marginBottom: 2 }}>
                {t('rooms.setup_login_title')}
              </div>
              <p style={{ margin: 0, color: 'var(--text-secondary)' }}>
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
          <RefreshIcon spinning={busy} size={15} />
          {busy ? t('rooms.setup_checking') : t('rooms.setup_check')}
        </Button>

        {waitingLogin && ts.auth_url && (
          <Button variant="secondary" onClick={() => { openUrl(ts.auth_url!); }}>
            <ExternalLink size={16} />
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
        <ShieldCheck size={14} aria-hidden style={{ verticalAlign: 'text-bottom', marginRight: 6 }} />
        {t('rooms.setup_secure')}
      </p>
    </div>
  );
}

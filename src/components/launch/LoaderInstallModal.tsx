import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { Modal } from '../ui/Modal';
import { ProgressBar } from '../ui/ProgressBar';
import { useT } from '../../lib/i18n';
import { useLogPlaque } from '../../lib/uiLog';

export interface LoaderCheckResult {
  needs_install: boolean;
  loader_type: string;
  loader_version: string;
  mc_version: string;
}

interface LoaderInstallModalProps {
  open: boolean;
  onClose: () => void;
  onInstalled: () => void;
  instanceName: string;
}

function parseProgress(message: string): { label: string; percent: number } {
  const match = message.match(/\((\d+)\/(\d+)\)/);
  if (match) {
    const current = parseInt(match[1], 10);
    const total = parseInt(match[2], 10);
    const shortName = message.replace(/\s*\(\d+\/\d+\)/, '').split(':').pop()?.trim() || message;
    return { label: `${shortName} (${current}/${total})`, percent: Math.round((current / total) * 100) };
  }
  return { label: message, percent: -1 };
}

export function LoaderInstallModal({ open, onClose, onInstalled, instanceName }: LoaderInstallModalProps) {
  const t = useT();
  const [stage, setStage] = useState<'idle' | 'installing' | 'done' | 'error'>('idle');
  const [message, setMessage] = useState('');
  const [error, setError] = useState('');
  const [checkResult, setCheckResult] = useState<LoaderCheckResult | null>(null);

  useLogPlaque(error, 'error', 'loader');
  useLogPlaque(stage === 'installing' || stage === 'done' ? message : null, 'info', 'loader');

  useEffect(() => {
    if (!open) return;

    let unlisten: (() => void) | null = null;

    listen<{ stage: string; message: string }>('loader-install-progress', (event) => {
      const { stage: s, message: msg } = event.payload;
      setMessage(msg);
      if (s === 'downloading' || s === 'extracting') {
        setStage('installing');
      } else if (s === 'done') {
        setStage('done');
        setTimeout(() => onInstalled(), 1500);
      } else if (s === 'error') {
        setStage('error');
        setError(msg);
      }
    }).then((unlistenFn) => { unlisten = unlistenFn; });

    return () => { unlisten?.(); };
  }, [open, onInstalled]);

  useEffect(() => {
    if (!open) return;
    setStage('idle');
    setMessage('');
    setError('');
    setCheckResult(null);

    invoke<LoaderCheckResult>('cmd_check_instance_loader', { instanceName })
      .then((result) => setCheckResult(result))
      .catch(() => setCheckResult(null));
  }, [open, instanceName]);

  const handleInstall = useCallback(async () => {
    setStage('installing');
    setMessage(t('loader_install.installing', { loader: checkResult?.loader_type || '' }));
    try {
      await invoke('cmd_install_instance_loader', { instanceName });
    } catch (e: any) {
      setStage('error');
      setError(e.toString());
    }
  }, [instanceName, checkResult, t]);

  if (!open) return null;

  const { label, percent } = message ? parseProgress(message) : { label: '', percent: -1 };

  return (
    <Modal
      open={open}
      onClose={stage === 'installing' ? () => {} : onClose}
      title={t('loader_install.title', { loader: checkResult?.loader_type || 'Mod Loader' })}
      footer={
        stage === 'idle' && checkResult?.needs_install ? (
          <>
            <button className="btn btn--ghost" onClick={onClose}>
              {t('updater.btn_later')}
            </button>
            <button className="btn btn--primary" onClick={handleInstall}>
              {t('loader_install.install_btn', { loader: checkResult?.loader_type || '' })}
            </button>
          </>
        ) : stage === 'error' ? (
          <>
            <button className="btn btn--ghost" onClick={onClose}>
              {t('common.cancel')}
            </button>
            <button className="btn btn--primary" onClick={handleInstall}>
              {t('loader_install.retry')}
            </button>
          </>
        ) : undefined
      }
    >
      <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--space-md)' }}>
        {stage === 'idle' && checkResult?.needs_install && (
          <p style={{ color: 'var(--text-secondary)', margin: 0 }}>
            {t('loader_install.description', {
              loader: checkResult.loader_type,
              version: checkResult.loader_version,
              mc: checkResult.mc_version,
            })}
          </p>
        )}

        {stage === 'installing' && (
          <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--space-sm)' }}>
            {percent >= 0 ? (
              <>
                <ProgressBar percent={percent} showLabel />
                <span style={{ color: 'var(--text-tertiary)', fontSize: 'var(--font-size-xs)' }}>
                  {label}
                </span>
              </>
            ) : (
              <>
                <span style={{ color: 'var(--text-secondary)', fontSize: 'var(--font-size-sm)' }}>
                  {message || t('common.installing')}
                </span>
                <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-sm)' }}>
                  <div className="spinner-sm" style={{ width: 16, height: 16 }} />
                </div>
              </>
            )}
          </div>
        )}

        {stage === 'done' && (
          <span style={{ color: 'var(--success)', fontSize: 'var(--font-size-sm)' }}>
            {t('loader_install.done', { loader: checkResult?.loader_type || '' })}
          </span>
        )}

        {stage === 'error' && (
          <span style={{ color: 'var(--error)', fontSize: 'var(--font-size-sm)' }}>
            {error}
          </span>
        )}
      </div>
    </Modal>
  );
}

import { useState, useEffect } from 'react';
import { listen } from '@tauri-apps/api/event';
import { Modal } from '../ui/Modal';
import { ProgressBar } from '../ui/ProgressBar';
import { useT } from '../../lib/i18n';

interface JavaProgress {
  major_version: number;
  percent: number;
  stage: string;
  message: string;
}

interface Props {
  open: boolean;
  onClose: () => void;
}

export function JavaSetupModal({ open, onClose }: Props) {
  const t = useT();
  const [progress, setProgress] = useState<JavaProgress | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!open) {
      setProgress(null);
      setError(null);
      return;
    }

    const unlisten = listen<JavaProgress>('java_download_progress', (event) => {
      const p = event.payload;
      if (p.stage === 'done') {
        setProgress(p);
        setTimeout(() => onClose(), 800);
      } else if (p.stage === 'error') {
        setError(p.message);
      } else {
        setProgress(p);
      }
    });

    return () => { unlisten.then((fn) => fn()).catch(() => {}); };
  }, [open, onClose]);

  if (!open) return null;

  const stageLabel = progress
    ? t(`launch.java_stage_${progress.stage}` as any, {}) || progress.message
    : t('launch.checking_java');

  return (
    <Modal open={open} onClose={onClose} title={t('launch.java_setup_title')} bare>
      <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--space-md)', minWidth: 320 }}>
        {error ? (
          <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--space-md)' }}>
            <span style={{ color: 'var(--error)', fontSize: 'var(--font-size-sm)' }}>
              {error}
            </span>
            <button className="btn btn--primary" onClick={onClose}>
              {t('common.close')}
            </button>
          </div>
        ) : progress ? (
          <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--space-sm)' }}>
            <span style={{ color: 'var(--text-secondary)', fontSize: 'var(--font-size-sm)' }}>
              {stageLabel}
            </span>
            <ProgressBar percent={progress.percent} showLabel />
            <span style={{ color: 'var(--text-tertiary)', fontSize: 'var(--font-size-xs)' }}>
              {progress.message}
            </span>
          </div>
        ) : (
          <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-sm)' }}>
            <div className="spinner-sm" style={{ width: 16, height: 16 }} />
            <span style={{ color: 'var(--text-secondary)', fontSize: 'var(--font-size-sm)' }}>
              {stageLabel}
            </span>
          </div>
        )}
      </div>
    </Modal>
  );
}

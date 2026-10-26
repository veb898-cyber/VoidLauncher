import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { Modal } from '../ui/Modal';
import { ProgressBar } from '../ui/ProgressBar';
import { SnakeSpinner } from '../ui/SnakeSpinner';
import { addToast } from '../ui/Toast';
import { useT, getLanguage } from '../../lib/i18n';
import { parseLoaderProgress } from '../../lib/loaderProgress';

interface LoaderVersion {
  version: string;
  stable: boolean;
}

interface LoaderVersionPage {
  versions: LoaderVersion[];
  total: number;
}

const LOADER_COMMANDS: Record<string, string> = {
  Fabric: 'cmd_get_fabric_versions',
  Forge: 'cmd_get_forge_versions',
  NeoForge: 'cmd_get_neoforge_versions',
};

const VERSION_LIMIT = 100;

interface LoaderVersionModalProps {
  open: boolean;
  instanceName: string;
  loader: string;
  mcVersion: string;
  currentVersion: string | null;
  onClose: () => void;
  onUpdated: (version: string) => void;
}

/** Loader version picker for an existing instance (the Prism-style
 *  "Change version" flow): pick a version from the list, the backend
 *  installs it and only then rewrites the instance, so a failed install
 *  leaves the previous loader untouched. */
export function LoaderVersionModal({
  open,
  instanceName,
  loader,
  mcVersion,
  currentVersion,
  onClose,
  onUpdated,
}: LoaderVersionModalProps) {
  const t = useT();
  const [versions, setVersions] = useState<LoaderVersion[]>([]);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState('');
  const [selected, setSelected] = useState<string | null>(null);
  const [stage, setStage] = useState<'idle' | 'installing' | 'done' | 'error'>('idle');
  const [message, setMessage] = useState('');
  const [error, setError] = useState('');

  const loadVersions = useCallback(async () => {
    const cmd = LOADER_COMMANDS[loader];
    if (!cmd) return;
    setLoading(true);
    setLoadError('');
    try {
      const page = await invoke<LoaderVersionPage>(cmd, {
        mcVersion,
        offset: 0,
        limit: VERSION_LIMIT,
      });
      setVersions(page.versions);
    } catch (e: any) {
      setLoadError(e.toString());
    } finally {
      setLoading(false);
    }
  }, [loader, mcVersion]);

  useEffect(() => {
    if (!open) return;
    setVersions([]);
    setSelected(null);
    setStage('idle');
    setMessage('');
    setError('');
    loadVersions();
  }, [open, loadVersions]);

  // Progress events from cmd_update_instance_loader (same channel as the
  // regular loader installs). Stage transitions for success/failure also
  // arrive through the invoke result below; the event just feeds progress.
  useEffect(() => {
    if (!open) return;
    let unlisten: (() => void) | null = null;
    listen<{ stage: string; message: string }>('loader-install-progress', (event) => {
      const { stage: s, message: msg } = event.payload;
      setMessage(msg);
      if (s === 'done') setStage('done');
      else if (s === 'error') {
        setStage('error');
        setError(msg);
      }
    }).then((unlistenFn) => { unlisten = unlistenFn; });
    return () => { unlisten?.(); };
  }, [open]);

  const handleInstall = useCallback(async () => {
    if (!selected) return;
    setStage('installing');
    setMessage(t('loader_install.installing', { loader }));
    try {
      await invoke('cmd_update_instance_loader', {
        instanceName,
        loaderVersion: selected,
        lang: getLanguage(),
      });
      setStage('done');
      addToast(t('loader_update.done_toast', { loader, version: selected }), 'success');
      setTimeout(() => onUpdated(selected), 1200);
    } catch (e: any) {
      setStage('error');
      setError(e.toString());
    }
  }, [instanceName, loader, selected, t, onUpdated]);

  if (!open) return null;

  const canInstall = !!selected && selected !== currentVersion && !loading && !loadError;
  const { label, percent } = message ? parseLoaderProgress(message) : { label: '', percent: -1 };

  return (
    <Modal
      open={open}
      onClose={stage === 'installing' ? () => {} : onClose}
      title={t('loader_update.title', { loader })}
      footer={
        stage === 'installing' || stage === 'done' ? undefined
        : stage === 'error' ? (
          <>
            <button className="btn btn--ghost" onClick={onClose}>
              {t('common.cancel')}
            </button>
            <button className="btn btn--primary" onClick={handleInstall} disabled={!selected}>
              {t('common.retry')}
            </button>
          </>
        ) : loadError ? (
          <>
            <button className="btn btn--ghost" onClick={onClose}>
              {t('common.cancel')}
            </button>
            <button className="btn btn--primary" onClick={loadVersions}>
              {t('common.retry')}
            </button>
          </>
        ) : (
          <>
            <button className="btn btn--ghost" onClick={onClose}>
              {t('common.cancel')}
            </button>
            <button className="btn btn--primary" onClick={handleInstall} disabled={!canInstall}>
              {selected ? t('loader_update.install_btn', { version: selected }) : t('common.install')}
            </button>
          </>
        )
      }
    >
      <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--space-md)' }}>
        <span style={{ fontSize: 'var(--font-size-sm)', color: 'var(--text-secondary)' }}>
          {currentVersion
            ? t('loader_update.current', { version: currentVersion })
            : t('loader_update.current_none')}
        </span>

        {stage === 'idle' && loading && (
          <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-sm)', color: 'var(--text-secondary)', fontSize: 'var(--font-size-sm)' }}>
            <SnakeSpinner size={16} /> {t('common.loading')}
          </div>
        )}

        {stage === 'idle' && !loading && loadError && (
          <span style={{ color: 'var(--error)', fontSize: 'var(--font-size-sm)' }}>
            {t('loader_update.load_error', { error: loadError })}
          </span>
        )}

        {stage === 'idle' && !loading && !loadError && (
          versions.length === 0 ? (
            <span style={{ color: 'var(--text-tertiary)', fontSize: 'var(--font-size-sm)' }}>
              {t('loader_update.empty', { mc: mcVersion })}
            </span>
          ) : (
            <div className="loader-version-list">
              {versions.map((v) => {
                const isCurrent = v.version === currentVersion;
                return (
                  <button
                    key={v.version}
                    type="button"
                    className={`loader-version-item${selected === v.version ? ' loader-version-item--selected' : ''}${isCurrent ? ' loader-version-item--current' : ''}`}
                    onClick={() => setSelected(v.version)}
                  >
                    <span>{v.version}</span>
                    <span className="loader-version-item__badges">
                      {isCurrent && (
                        <span className="loader-version-item__badge loader-version-item__badge--current">
                          {t('loader_update.installed')}
                        </span>
                      )}
                      {v.stable && (
                        <span className="loader-version-item__badge">{t('loader_update.stable')}</span>
                      )}
                    </span>
                  </button>
                );
              })}
            </div>
          )
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
                  <SnakeSpinner size={18} />
                </div>
              </>
            )}
          </div>
        )}

        {stage === 'done' && (
          <span style={{ color: 'var(--success)', fontSize: 'var(--font-size-sm)' }}>
            {t('loader_update.done_toast', { loader, version: selected ?? '' })}
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

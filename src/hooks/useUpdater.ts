import { useState, useEffect, useCallback, useRef } from 'react';
import { check } from '@tauri-apps/plugin-updater';
import { relaunch } from '@tauri-apps/plugin-process';
import { addToast } from '../components/ui/Toast';
import { t } from '../lib/i18n';

export interface UpdateInfo {
  version: string;
  body: string;
}

export interface UpdaterState {
  checking: boolean;
  updateAvailable: boolean;
  updateInfo: UpdateInfo | null;
  downloading: boolean;
  downloadProgress: number;
  installing: boolean;
  error: string | null;
  checkError: string | null;
}

export function useUpdater() {
  const [state, setState] = useState<UpdaterState>({
    checking: false,
    updateAvailable: false,
    updateInfo: null,
    downloading: false,
    downloadProgress: 0,
    installing: false,
    error: null,
    checkError: null,
  });

  const skipCheck = useRef(false);

  const checkForUpdates = useCallback(async () => {
    if (skipCheck.current) return;
    setState((s) => ({ ...s, checking: true, checkError: null }));
    try {
      const update = await check();
      if (update) {
        setState((s) => ({
          ...s,
          checking: false,
          updateAvailable: true,
          updateInfo: { version: update.version, body: update.body ?? '' },
        }));
      } else {
        setState((s) => ({ ...s, checking: false }));
      }
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      console.error('Update check failed:', err);
      setState((s) => ({ ...s, checking: false, checkError: msg }));
    }
  }, []);

  const retryCheck = useCallback(async () => {
    setState((s) => ({ ...s, checkError: null }));
    skipCheck.current = false;
    await checkForUpdates();
  }, [checkForUpdates]);

  const dismissCheckError = useCallback(() => {
    skipCheck.current = true;
    setState((s) => ({ ...s, checkError: null }));
  }, []);

  const dismissUpdate = useCallback(() => {
    setState((s) => ({ ...s, updateAvailable: false, updateInfo: null }));
  }, []);

  const downloadAndInstall = useCallback(async () => {
    setState((s) => ({ ...s, downloading: true, downloadProgress: 0, error: null }));

    try {
      const update = await check();
      if (!update) {
        setState((s) => ({ ...s, downloading: false, updateAvailable: false }));
        return;
      }

    let downloaded = 0;
    let contentLength = 0;
    let lastReportedPct = -1;

    await update.downloadAndInstall((event) => {
      switch (event.event) {
        case 'Started':
          contentLength = event.data.contentLength ?? 0;
          break;
        case 'Progress':
          downloaded += event.data.chunkLength;
          if (contentLength > 0) {
            const pct = Math.min(99, Math.round((downloaded / contentLength) * 100));
            if (pct !== lastReportedPct) {
              lastReportedPct = pct;
              setState((s) => ({ ...s, downloadProgress: pct }));
            }
          }
          break;
        case 'Finished':
          break;
      }
    });

    setState((s) => ({ ...s, downloadProgress: 100 }));
    setState((s) => ({ ...s, downloading: false, installing: true }));

    await relaunch();
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      console.error('Update failed:', err);
      setState((s) => ({ ...s, downloading: false, installing: false, error: msg }));
      addToast(t('updater.error', { error: msg }), 'error');
    }
  }, []);

  useEffect(() => {
    const timer = setTimeout(checkForUpdates, 3000);
    return () => clearTimeout(timer);
  }, [checkForUpdates]);

  return {
    ...state,
    checkForUpdates,
    retryCheck,
    dismissCheckError,
    dismissUpdate,
    downloadAndInstall,
  };
}

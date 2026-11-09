import { useState, useEffect, useCallback, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Update } from '@tauri-apps/plugin-updater';
import { relaunch } from '@tauri-apps/plugin-process';
import { addToast } from '../components/ui/Toast';
import { t } from '../lib/i18n';
import {
  checkUpdaterWithFallback,
  downloadUpdaterWithFallback,
  UpdaterProxyUnavailableError,
  type UpdaterCheckOptions,
  type UpdaterProxyPolicy,
} from '../lib/updaterProxy';

interface UpdaterCheckMetadata {
  rid: number;
  currentVersion: string;
  version: string;
  body?: string | null;
  rawJson: Record<string, unknown>;
}

/**
 * The Tauri updater plugin has its own HTTP stack, so a small backend command
 * builds it explicitly with `proxy(url)` or `no_proxy()` and returns the
 * plugin's own `Update` resource. Signature verification and installer
 * validation remain the plugin's job.
 */
async function updaterProxyPolicy(): Promise<UpdaterProxyPolicy> {
  return invoke<UpdaterProxyPolicy>('cmd_updater_proxy_policy');
}

async function checkUpdater(options: UpdaterCheckOptions): Promise<Update | null> {
  const metadata = await invoke<UpdaterCheckMetadata | null>('cmd_updater_check', {
    mode: options.mode,
  });
  if (!metadata) return null;
  return new Update({ ...metadata, body: metadata.body ?? undefined });
}

function updaterErrorMessage(error: unknown): string {
  if (error instanceof UpdaterProxyUnavailableError) {
    return t('updater.proxy_unavailable');
  }
  return error instanceof Error ? error.message : String(error);
}

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
  const operationInFlight = useRef(false);

  const checkForUpdates = useCallback(async () => {
    if (skipCheck.current || operationInFlight.current) return;
    setState((s) => ({ ...s, checking: true, checkError: null }));
    try {
      const policy = await updaterProxyPolicy();
      const update = await checkUpdaterWithFallback(policy, checkUpdater);
      if (update) {
        const info = { version: update.version, body: update.body ?? '' };
        await update.close().catch(() => {});
        setState((s) => ({
          ...s,
          checking: false,
          updateAvailable: true,
          updateInfo: info,
        }));
      } else {
        setState((s) => ({ ...s, checking: false }));
      }
    } catch (err) {
      const msg = updaterErrorMessage(err);
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
    if (operationInFlight.current) return;
    operationInFlight.current = true;
    setState((s) => ({ ...s, downloading: true, downloadProgress: 0, error: null }));

    try {
      const policy = await updaterProxyPolicy();
      let downloaded = 0;
      let contentLength = 0;
      let lastReportedPct = -1;

      const installed = await downloadUpdaterWithFallback(policy, {
        check: checkUpdater,
        download: async (update) => {
          // A retry starts a fresh download; do not carry the previous
          // attempt's byte count into the new progress bar.
          downloaded = 0;
          contentLength = 0;
          lastReportedPct = -1;
          setState((s) => ({ ...s, downloadProgress: 0 }));

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
        },
        release: (update) => update.close(),
      });

      if (!installed) {
        setState((s) => ({ ...s, downloading: false, updateAvailable: false }));
        return;
      }

      setState((s) => ({ ...s, downloadProgress: 100 }));
      setState((s) => ({ ...s, downloading: false, installing: true }));

      await relaunch();
    } catch (err) {
      const msg = updaterErrorMessage(err);
      console.error('Update failed:', err);
      setState((s) => ({ ...s, downloading: false, installing: false, error: msg }));
      addToast(t('updater.error', { error: msg }), 'error');
    } finally {
      operationInFlight.current = false;
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

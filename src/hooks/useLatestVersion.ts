import { useEffect, useState, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';

declare const __APP_VERSION__: string;

export const APP_VERSION: string =
  typeof __APP_VERSION__ !== 'undefined' ? __APP_VERSION__ : '0.0.0';

export interface LatestVersionState {
  /** Latest version published on the `main` branch (e.g. "0.1.7"). */
  latest: string | null;
  /** True while a fetch is in flight. */
  loading: boolean;
  /** Last error message, or null. */
  error: string | null;
  /** Re-fetch the manifest. */
  refresh: () => Promise<void>;
  /** True once the initial fetch has completed (success or failure). */
  checked: boolean;
}

export interface VersionComparison {
  /** -1: current < latest, 0: equal, 1: current > latest. null: unknown. */
  status: -1 | 0 | 1 | null;
  /** True if `current` is older than `latest`. */
  updateAvailable: boolean;
}

/**
 * Compare two `MAJOR.MINOR.PATCH` version strings. Returns -1 / 0 / 1, or
 * null if either string can't be parsed. Pre-release tags are stripped so
 * "0.1.7-rc1" compares equal to "0.1.7".
 */
export function compareVersions(a: string, b: string): -1 | 0 | 1 | null {
  const parse = (v: string) => {
    const m = v.trim().replace(/^v/i, '').split('-')[0].split('+')[0].split('.');
    if (m.length === 0 || m.some((p) => !/^\d+$/.test(p))) return null;
    return m.map((p) => parseInt(p, 10));
  };
  const pa = parse(a);
  const pb = parse(b);
  if (!pa || !pb) return null;
  const len = Math.max(pa.length, pb.length);
  for (let i = 0; i < len; i++) {
    const ai = pa[i] ?? 0;
    const bi = pb[i] ?? 0;
    if (ai < bi) return -1;
    if (ai > bi) return 1;
  }
  return 0;
}

export function getVersionComparison(
  current: string,
  latest: string | null,
): VersionComparison {
  if (!latest) return { status: null, updateAvailable: false };
  const status = compareVersions(current, latest);
  if (status === null) return { status: null, updateAvailable: false };
  return { status, updateAvailable: status === -1 };
}

export function useLatestVersion(): LatestVersionState {
  const [latest, setLatest] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [checked, setChecked] = useState(false);

  const refresh = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      // Fetched through the backend HTTP stack (proxy with direct fallback):
      // a webview fetch would follow only the system proxy, so users whose
      // proxy blocks raw.githubusercontent.com never saw update checks.
      const v = await invoke<string | null>('cmd_check_latest_version');
      if (v) {
        setLatest(v);
      } else {
        setLatest(null);
        setError('Invalid manifest');
      }
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setError(msg);
      setLatest(null);
    } finally {
      setLoading(false);
      setChecked(true);
    }
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  return { latest, loading, error, refresh, checked };
}

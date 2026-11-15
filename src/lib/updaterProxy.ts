/**
 * Network policy for the Tauri updater plugin.
 *
 * The plugin runs its own HTTP stack. Its JS `check` only accepts a proxy URL
 * and cannot force a direct connection, so the backend command
 * `cmd_updater_check` builds the plugin updater explicitly with either
 * `proxy(url)` or `no_proxy()` and returns the plugin's own `Update` resource.
 * This file decides which attempts are allowed and coordinates both stages:
 * checking for an update and downloading/installing it.
 *
 * The plugin's update object still performs the normal minisign signature
 * check and installer validation — none of that is bypassed here.
 *
 * Modes:
 * - `direct` — “Use proxy” is off (or the proxy is unusable in fallback mode):
 *   only a direct attempt is allowed.
 * - `proxy` — “Use proxy” and “Use proxy only” are on: only the proxy attempt
 *   is allowed.
 * - `proxy-fallback` — proxy is on, “Use proxy only” is off: proxy first, and
 *   a direct attempt only after that proxy attempt failed.
 * - `unavailable` — strict proxy mode but no proxy is configured/usable: no
 *   network attempt at all.
 */

export interface UpdaterProxyPolicy {
  /** Master “Use proxy” setting. */
  enabled: boolean;
  /** “Use proxy only” (forbid direct fallback). Only meaningful when enabled. */
  proxyOnly: boolean;
  /** Resolved proxy URL, or null when no usable proxy is configured. */
  proxyUrl: string | null;
}

export interface UpdaterCheckOptions {
  mode: 'direct' | 'proxy';
  proxyUrl?: string;
}

export type UpdaterProxyMode = 'direct' | 'proxy' | 'proxy-fallback' | 'unavailable';

export interface UpdaterProxyPlan {
  mode: UpdaterProxyMode;
  proxyUrl?: string;
}

/** Strict proxy mode could not reach the proxy; no direct request was made. */
export class UpdaterProxyUnavailableError extends Error {
  constructor(readonly originalError?: unknown) {
    super('Proxy-only mode is enabled, but no proxy is configured or usable.');
    this.name = 'UpdaterProxyUnavailableError';
  }
}

/** Pure decision table for the three proxy modes (and the missing-proxy case). */
export function planUpdaterProxy(policy: UpdaterProxyPolicy): UpdaterProxyPlan {
  // The master switch always wins: a saved “Use proxy only” value must not
  // route traffic through a proxy the user disabled.
  if (!policy.enabled) return { mode: 'direct' };

  const proxyUrl = policy.proxyUrl?.trim();
  if (!proxyUrl) {
    // The proxy attempt is considered failed when nothing is configured.
    return policy.proxyOnly ? { mode: 'unavailable' } : { mode: 'direct' };
  }
  return policy.proxyOnly
    ? { mode: 'proxy', proxyUrl }
    : { mode: 'proxy-fallback', proxyUrl };
}

async function releaseQuietly<T>(
  update: T | null,
  release?: (update: T) => Promise<void>,
) {
  if (!update || !release) return;
  try {
    await release(update);
  } catch {
    // Resource cleanup must never mask the update result/error.
  }
}

/**
 * Execute the check stage using the allowed attempts. `check` is injected so
 * tests can observe the exact sequence without touching the network or the
 * plugin. The returned value is the plugin's `Update` object (or null).
 */
export async function checkUpdaterWithFallback<T>(
  policy: UpdaterProxyPolicy,
  check: (options: UpdaterCheckOptions) => Promise<T | null>,
): Promise<T | null> {
  const plan = planUpdaterProxy(policy);
  switch (plan.mode) {
    case 'direct':
      return check({ mode: 'direct' });
    case 'proxy':
      try {
        return await check({ mode: 'proxy', proxyUrl: plan.proxyUrl });
      } catch (error) {
        // Strict mode: the proxy attempt is the only allowed one. Surface a
        // stable error the UI can localize instead of the plugin's raw text.
        throw new UpdaterProxyUnavailableError(error);
      }
    case 'proxy-fallback':
      try {
        return await check({ mode: 'proxy', proxyUrl: plan.proxyUrl });
      } catch (proxyError) {
        try {
          return await check({ mode: 'direct' });
        } catch {
          // Mirror the rest of the launcher: the original proxy error is the
          // useful one when both attempts fail.
          throw proxyError;
        }
      }
    case 'unavailable':
      throw new UpdaterProxyUnavailableError();
  }
}

export interface UpdaterDownloadAttempts<T> {
  check: (options: UpdaterCheckOptions) => Promise<T | null>;
  download: (update: T) => Promise<void>;
  /** Optional resource cleanup; called after each attempt's update is spent. */
  release?: (update: T) => Promise<void>;
}

/**
 * Check **and download/install** using the same policy. This is deliberately
 * separate from the check-only helper: a proxy can successfully serve the
 * update manifest while failing the installer download, and fallback must also
 * cover that second stage. A direct retry obtains a fresh plugin `Update`
 * object built with `no_proxy()`, so the installer download itself is direct.
 */
export async function downloadUpdaterWithFallback<T>(
  policy: UpdaterProxyPolicy,
  attempts: UpdaterDownloadAttempts<T>,
): Promise<boolean> {
  const plan = planUpdaterProxy(policy);

  const attempt = async (options: UpdaterCheckOptions): Promise<boolean> => {
    const update = await attempts.check(options);
    if (!update) return false;
    try {
      await attempts.download(update);
      return true;
    } finally {
      await releaseQuietly(update, attempts.release);
    }
  };

  switch (plan.mode) {
    case 'direct':
      return attempt({ mode: 'direct' });
    case 'proxy':
      try {
        return await attempt({ mode: 'proxy', proxyUrl: plan.proxyUrl });
      } catch (error) {
        throw new UpdaterProxyUnavailableError(error);
      }
    case 'proxy-fallback':
      try {
        return await attempt({ mode: 'proxy', proxyUrl: plan.proxyUrl });
      } catch (proxyError) {
        try {
          if (await attempt({ mode: 'direct' })) return true;
        } catch {
          // Preserve the first (proxy) error, matching the launcher's HTTP
          // fallback behaviour.
        }
        // The proxy stage had an update/download failure; a direct check that
        // reports "no update" must not hide that failure.
        throw proxyError;
      }
    case 'unavailable':
      throw new UpdaterProxyUnavailableError();
  }
}

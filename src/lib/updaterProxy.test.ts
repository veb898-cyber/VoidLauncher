import { describe, expect, it, vi } from 'vitest';
import {
  checkUpdaterWithFallback,
  downloadUpdaterWithFallback,
  planUpdaterProxy,
  UpdaterProxyUnavailableError,
  type UpdaterCheckOptions,
  type UpdaterProxyPolicy,
} from './updaterProxy';

const policy = (
  enabled: boolean,
  proxyOnly: boolean,
  proxyUrl: string | null = 'http://127.0.0.1:8080',
): UpdaterProxyPolicy => ({ enabled, proxyOnly, proxyUrl });

const proxyOptions: UpdaterCheckOptions = {
  mode: 'proxy',
  proxyUrl: 'http://127.0.0.1:8080',
};
const directOptions: UpdaterCheckOptions = { mode: 'direct' };

describe('updater proxy plan', () => {
  it('uses direct only when the master proxy switch is off, even if proxy-only was saved on', () => {
    expect(planUpdaterProxy(policy(false, true))).toEqual({ mode: 'direct' });
    expect(planUpdaterProxy(policy(false, false))).toEqual({ mode: 'direct' });
  });

  it('uses only the proxy in strict mode', () => {
    expect(planUpdaterProxy(policy(true, true))).toEqual({
      mode: 'proxy',
      proxyUrl: 'http://127.0.0.1:8080',
    });
  });

  it('uses proxy with a direct fallback when strict mode is off', () => {
    expect(planUpdaterProxy(policy(true, false))).toEqual({
      mode: 'proxy-fallback',
      proxyUrl: 'http://127.0.0.1:8080',
    });
  });

  it('refuses to start in strict mode without a usable proxy', () => {
    expect(planUpdaterProxy(policy(true, true, null))).toEqual({ mode: 'unavailable' });
    expect(planUpdaterProxy(policy(true, true, '   '))).toEqual({ mode: 'unavailable' });
  });

  it('treats a missing proxy as a failed proxy attempt in fallback mode', () => {
    expect(planUpdaterProxy(policy(true, false, null))).toEqual({ mode: 'direct' });
  });
});

describe('updater check attempts', () => {
  it('never uses proxy mode when the master switch is off', async () => {
    const check = vi.fn().mockResolvedValue('direct-update');

    await expect(
      checkUpdaterWithFallback(policy(false, true), check),
    ).resolves.toBe('direct-update');
    expect(check).toHaveBeenCalledTimes(1);
    expect(check).toHaveBeenCalledWith(directOptions);
  });

  it('does not fall back to direct after a proxy failure in strict mode', async () => {
    const check = vi.fn().mockRejectedValue(new Error('proxy down'));

    await expect(
      checkUpdaterWithFallback(policy(true, true), check),
    ).rejects.toBeInstanceOf(UpdaterProxyUnavailableError);
    expect(check).toHaveBeenCalledTimes(1);
    expect(check).toHaveBeenCalledWith(proxyOptions);
  });

  it('makes no network attempt at all in strict mode without a proxy', async () => {
    const check = vi.fn();

    await expect(
      checkUpdaterWithFallback(policy(true, true, null), check),
    ).rejects.toBeInstanceOf(UpdaterProxyUnavailableError);
    expect(check).not.toHaveBeenCalled();
  });

  it('uses the proxy first and does not retry direct when it succeeds', async () => {
    const check = vi.fn().mockResolvedValue('proxied-update');

    await expect(
      checkUpdaterWithFallback(policy(true, false), check),
    ).resolves.toBe('proxied-update');
    expect(check).toHaveBeenCalledTimes(1);
    expect(check).toHaveBeenCalledWith(proxyOptions);
  });

  it('retries directly after a proxy check failure when fallback is allowed', async () => {
    const calls: Array<UpdaterCheckOptions> = [];
    const check = vi.fn().mockImplementation(async (options: UpdaterCheckOptions) => {
      calls.push(options);
      if (options.mode === 'proxy') throw new Error('proxy down');
      return 'direct-update';
    });

    await expect(
      checkUpdaterWithFallback(policy(true, false), check),
    ).resolves.toBe('direct-update');
    expect(calls).toEqual([proxyOptions, directOptions]);
  });
});

describe('updater download attempts', () => {
  interface FakeUpdate {
    version: string;
    download: ReturnType<typeof vi.fn>;
  }

  const update = (version = '1.6.0'): FakeUpdate => ({
    version,
    download: vi.fn(),
  });

  it('downloads through the proxy and never retries direct when it succeeds', async () => {
    const proxyUpdate = update();
    const directUpdate = update();
    const check = vi.fn().mockImplementation(async (options: UpdaterCheckOptions) =>
      options.mode === 'proxy' ? proxyUpdate : directUpdate,
    );
    const download = vi.fn().mockResolvedValue(undefined);
    const release = vi.fn().mockResolvedValue(undefined);

    await expect(
      downloadUpdaterWithFallback(policy(true, false), { check, download, release }),
    ).resolves.toBe(true);

    expect(check).toHaveBeenCalledTimes(1);
    expect(check).toHaveBeenCalledWith(proxyOptions);
    expect(download).toHaveBeenCalledTimes(1);
    expect(download).toHaveBeenCalledWith(proxyUpdate);
    expect(release).toHaveBeenCalledWith(proxyUpdate);
    expect(download).not.toHaveBeenCalledWith(directUpdate);
  });

  it('retries the installer download directly after a proxy download failure', async () => {
    const proxyUpdate = update();
    const directUpdate = update();
    const check = vi.fn().mockImplementation(async (options: UpdaterCheckOptions) =>
      options.mode === 'proxy' ? proxyUpdate : directUpdate,
    );
    const download = vi.fn().mockImplementation(async (candidate: FakeUpdate) => {
      if (candidate === proxyUpdate) throw new Error('proxy download failed');
    });
    const release = vi.fn().mockResolvedValue(undefined);

    await expect(
      downloadUpdaterWithFallback(policy(true, false), { check, download, release }),
    ).resolves.toBe(true);

    expect(check.mock.calls.map(([options]) => (options as UpdaterCheckOptions).mode)).toEqual([
      'proxy',
      'direct',
    ]);
    expect(download.mock.calls.map(([candidate]) => candidate)).toEqual([
      proxyUpdate,
      directUpdate,
    ]);
    expect(release).toHaveBeenCalledWith(proxyUpdate);
    expect(release).toHaveBeenCalledWith(directUpdate);
  });

  it('does not allow a direct installer retry in strict mode', async () => {
    const proxyUpdate = update();
    const check = vi.fn().mockResolvedValue(proxyUpdate);
    const download = vi.fn().mockRejectedValue(new Error('proxy download failed'));
    const release = vi.fn().mockResolvedValue(undefined);

    await expect(
      downloadUpdaterWithFallback(policy(true, true), { check, download, release }),
    ).rejects.toBeInstanceOf(UpdaterProxyUnavailableError);

    expect(check).toHaveBeenCalledTimes(1);
    expect(check).toHaveBeenCalledWith(proxyOptions);
    expect(download).toHaveBeenCalledTimes(1);
    expect(release).toHaveBeenCalledWith(proxyUpdate);
  });

  it('falls back to a direct check and download when the proxy check fails', async () => {
    const directUpdate = update();
    const check = vi.fn().mockImplementation(async (options: UpdaterCheckOptions) => {
      if (options.mode === 'proxy') throw new Error('proxy down');
      return directUpdate;
    });
    const download = vi.fn().mockResolvedValue(undefined);
    const release = vi.fn().mockResolvedValue(undefined);

    await expect(
      downloadUpdaterWithFallback(policy(true, false), { check, download, release }),
    ).resolves.toBe(true);

    expect(download).toHaveBeenCalledWith(directUpdate);
    expect(release).toHaveBeenCalledWith(directUpdate);
  });

  it('uses only direct check/download when the master switch is off', async () => {
    const directUpdate = update();
    const check = vi.fn().mockResolvedValue(directUpdate);
    const download = vi.fn().mockResolvedValue(undefined);
    const release = vi.fn().mockResolvedValue(undefined);

    await expect(
      downloadUpdaterWithFallback(policy(false, true), { check, download, release }),
    ).resolves.toBe(true);

    expect(check).toHaveBeenCalledTimes(1);
    expect(check).toHaveBeenCalledWith(directOptions);
    expect(download).toHaveBeenCalledWith(directUpdate);
    expect(release).toHaveBeenCalledWith(directUpdate);
  });

  it('surfaces the proxy download error when the direct retry also fails', async () => {
    const proxyUpdate = update();
    const directUpdate = update();
    const proxyError = new Error('proxy download failed');
    const check = vi.fn().mockImplementation(async (options: UpdaterCheckOptions) =>
      options.mode === 'proxy' ? proxyUpdate : directUpdate,
    );
    const download = vi.fn().mockImplementation(async (candidate: FakeUpdate) => {
      if (candidate === proxyUpdate) throw proxyError;
      throw new Error('direct download failed');
    });
    const release = vi.fn().mockResolvedValue(undefined);

    await expect(
      downloadUpdaterWithFallback(policy(true, false), { check, download, release }),
    ).rejects.toThrow('proxy download failed');

    expect(release).toHaveBeenCalledWith(proxyUpdate);
    expect(release).toHaveBeenCalledWith(directUpdate);
  });

  it('does not hide a proxy download failure when the direct check finds no update', async () => {
    const proxyUpdate = update();
    const proxyError = new Error('proxy download failed');
    const check = vi.fn().mockImplementation(async (options: UpdaterCheckOptions) =>
      options.mode === 'proxy' ? proxyUpdate : null,
    );
    const download = vi.fn().mockRejectedValue(proxyError);
    const release = vi.fn().mockResolvedValue(undefined);

    await expect(
      downloadUpdaterWithFallback(policy(true, false), { check, download, release }),
    ).rejects.toThrow('proxy download failed');
    expect(release).toHaveBeenCalledWith(proxyUpdate);
  });

  it('passes the plugin update object to its own download path unchanged', async () => {
    const proxyUpdate = update();
    const check = vi.fn().mockResolvedValue(proxyUpdate);
    const download = vi.fn().mockImplementation(async (candidate: FakeUpdate) => {
      // Signature/installer validation stays inside the plugin object; the
      // helper only invokes the same `downloadAndInstall` it exposes.
      expect(candidate).toBe(proxyUpdate);
    });

    await expect(
      downloadUpdaterWithFallback(policy(true, true), { check, download }),
    ).resolves.toBe(true);
    expect(download).toHaveBeenCalledWith(proxyUpdate);
  });
});

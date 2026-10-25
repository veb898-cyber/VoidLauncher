import { describe, expect, it } from 'vitest';
import { needsLoaderVersionFetch } from './loaderVersions';

const idle = { total: null, loading: false, failed: false };

describe('needsLoaderVersionFetch', () => {
  it('retries Forge once the Minecraft version has loaded', () => {
    expect(
      needsLoaderVersionFetch({ ...idle, loader: 'Forge', mcVersion: '1.21.1' }),
    ).toBe(true);
  });

  it('waits for the Minecraft version before requesting Forge/NeoForge', () => {
    expect(
      needsLoaderVersionFetch({ ...idle, loader: 'Forge', mcVersion: '' }),
    ).toBe(false);
    expect(
      needsLoaderVersionFetch({ ...idle, loader: 'NeoForge', mcVersion: '' }),
    ).toBe(false);
  });

  it('does not request Vanilla loader versions', () => {
    expect(
      needsLoaderVersionFetch({ ...idle, loader: 'Vanilla', mcVersion: '1.21.1' }),
    ).toBe(false);
  });

  it('does not re-request a page that already loaded (including an empty one)', () => {
    expect(
      needsLoaderVersionFetch({
        ...idle,
        loader: 'Forge',
        mcVersion: '1.21.1',
        total: 0,
      }),
    ).toBe(false);
    expect(
      needsLoaderVersionFetch({
        ...idle,
        loader: 'Forge',
        mcVersion: '1.21.1',
        total: 12,
      }),
    ).toBe(false);
  });

  it('does not stack requests or loop after a failure', () => {
    expect(
      needsLoaderVersionFetch({
        ...idle,
        loader: 'NeoForge',
        mcVersion: '1.21.1',
        loading: true,
      }),
    ).toBe(false);
    expect(
      needsLoaderVersionFetch({
        ...idle,
        loader: 'NeoForge',
        mcVersion: '1.21.1',
        failed: true,
      }),
    ).toBe(false);
  });

  it('waits for the Minecraft version for Fabric too', () => {
    expect(
      needsLoaderVersionFetch({ ...idle, loader: 'Fabric', mcVersion: '' }),
    ).toBe(false);
    expect(
      needsLoaderVersionFetch({ ...idle, loader: 'Fabric', mcVersion: '1.21.1' }),
    ).toBe(true);
  });
});

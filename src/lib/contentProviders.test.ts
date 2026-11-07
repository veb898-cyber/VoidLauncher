import { describe, expect, it } from 'vitest';
import { isCurrentProviderRequest, type ProviderRequestToken } from './contentProviders';

describe('isCurrentProviderRequest', () => {
  it('stale_add_latest_after_provider_switch_is_rejected', () => {
    const started: ProviderRequestToken = { provider: 'curseforge', generation: 4 };
    const current: ProviderRequestToken = { provider: 'modrinth', generation: 5 };

    expect(isCurrentProviderRequest(started, current)).toBe(false);
  });

  it('accepts a result while the provider and generation are unchanged', () => {
    const started: ProviderRequestToken = { provider: 'modrinth', generation: 7 };
    const current: ProviderRequestToken = { provider: 'modrinth', generation: 7 };

    expect(isCurrentProviderRequest(started, current)).toBe(true);
  });

  it('rejects a result after a rapid switch away and back', () => {
    const started: ProviderRequestToken = { provider: 'curseforge', generation: 2 };
    const current: ProviderRequestToken = { provider: 'curseforge', generation: 4 };

    expect(isCurrentProviderRequest(started, current)).toBe(false);
  });
});

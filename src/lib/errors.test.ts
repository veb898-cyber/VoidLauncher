import { describe, expect, it } from 'vitest';
import { friendlyErrorMessage } from './errors';
import type { MessageKey } from './i18n';

const t = (key: MessageKey) => `t:${key}`;

describe('friendlyErrorMessage', () => {
  it('maps network failures', () => {
    expect(friendlyErrorMessage(new Error('connection reset by peer'), t)).toBe(
      't:errors.network',
    );
    expect(friendlyErrorMessage('request timed out', t)).toBe('t:errors.network');
  });

  it('maps access, disk and launch failures', () => {
    expect(friendlyErrorMessage('Access is denied. (os error 5)', t)).toBe(
      't:errors.access',
    );
    expect(friendlyErrorMessage('No space left on device', t)).toBe('t:errors.disk');
    expect(
      friendlyErrorMessage('Could not find or load main class net.minecraft.client.Main', t),
    ).toBe('t:errors.main_class');
  });

  it('passes unrecognized errors through unchanged', () => {
    expect(friendlyErrorMessage(new Error('weird custom failure'), t)).toBe(
      'weird custom failure',
    );
    expect(friendlyErrorMessage('plain string failure', t)).toBe('plain string failure');
  });
});

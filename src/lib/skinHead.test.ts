import { describe, expect, it } from 'vitest';
import { skinHeadCacheKey } from './skinHead';

describe('skinHeadCacheKey', () => {
  it('changes when the account skin changes', () => {
    const uuid = '1a650540-fb59-4d9d-a1d3-f8f7a67379a3';
    const oldSkin = 'data:image/png;base64,OLD_SKIN';
    const newSkin = 'data:image/png;base64,NEW_SKIN';

    expect(skinHeadCacheKey(uuid, oldSkin)).not.toBe(skinHeadCacheKey(uuid, newSkin));
  });

  it('is stable for the same account and identical skin bytes', () => {
    const uuid = '1a650540fb594d9da1d3f8f7a67379a3';
    const skin = 'data:image/png;base64,SAME_SKIN';

    expect(skinHeadCacheKey(uuid, skin)).toBe(skinHeadCacheKey(uuid, skin));
    expect(skinHeadCacheKey(uuid, skin)).toBe(
      skinHeadCacheKey('1A650540-FB59-4D9D-A1D3-F8F7A67379A3', skin),
    );
  });
});

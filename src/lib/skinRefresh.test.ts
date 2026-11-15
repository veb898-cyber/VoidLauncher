import { describe, expect, it } from 'vitest';
import { canRefreshSkin } from './skinRefresh';

describe('skin refresh eligibility', () => {
  it('offers refresh for Microsoft and Ely.by accounts', () => {
    expect(canRefreshSkin('Microsoft')).toBe(true);
    expect(canRefreshSkin('ElyBy')).toBe(true);
  });

  it('never offers refresh for offline accounts', () => {
    expect(canRefreshSkin('Offline')).toBe(false);
    expect(canRefreshSkin(undefined)).toBe(false);
  });
});

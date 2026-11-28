import { describe, expect, it } from 'vitest';
import {
  CompatibilityRequestTracker,
  countCompatibilityFindings,
  filterCompatibilityFindings,
  type LocalCompatibilityFinding,
} from './localCompatibility';

const finding = (status: LocalCompatibilityFinding['status']): LocalCompatibilityFinding => ({
  code: `test_${status}`,
  status,
  source: 'fabric.mod.json',
  basis: 'fixture',
  explanation: 'fixture finding',
  affectedFiles: ['mod.jar'],
  affectedModIds: ['example'],
});

describe('local compatibility report helpers', () => {
  it('counts and filters each certainty category', () => {
    const findings = [finding('confirmed'), finding('potential'), finding('unknown'), finding('unknown')];

    expect(countCompatibilityFindings(findings)).toEqual({ total: 4, confirmed: 1, potential: 1, unknown: 2 });
    expect(filterCompatibilityFindings(findings, 'confirmed')).toEqual([findings[0]]);
    expect(filterCompatibilityFindings(findings, 'potential')).toEqual([findings[1]]);
    expect(filterCompatibilityFindings(findings, 'unknown')).toEqual([findings[2], findings[3]]);
    expect(filterCompatibilityFindings(findings, 'all')).toBe(findings);
  });

  it('coalesces duplicate requests and lets invalidation supersede an older response', () => {
    const requests = new CompatibilityRequestTracker();
    const first = requests.begin();

    expect(first).not.toBeNull();
    expect(requests.begin()).toBeNull();

    requests.invalidate();
    const current = requests.begin();
    expect(current).not.toBeNull();
    expect(requests.isCurrent(first!)).toBe(false);
    expect(requests.isCurrent(current!)).toBe(true);

    requests.finish(first!);
    expect(requests.begin()).toBeNull();
    requests.finish(current!);
    expect(requests.begin()).not.toBeNull();
  });
});

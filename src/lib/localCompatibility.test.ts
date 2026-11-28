import { describe, expect, it } from 'vitest';
import {
  CompatibilityRequestTracker,
  DeferredCompatibilityRefresh,
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

  it('serializes a fresh scan after invalidating an in-flight scan', () => {
    const requests = new CompatibilityRequestTracker();
    const first = requests.begin();

    expect(first).not.toBeNull();
    expect(requests.begin()).toBeNull();

    requests.invalidate();
    expect(requests.begin()).toBeNull();
    expect(requests.isCurrent(first!)).toBe(false);

    expect(requests.finish(first!)).toBe(true);
    const current = requests.begin();
    expect(current).not.toBeNull();
    expect(requests.isCurrent(current!)).toBe(true);
    expect(requests.finish(current!)).toBe(false);
  });

  it('coalesces watcher changes until ContentManager can reload', () => {
    const refresh = new DeferredCompatibilityRefresh();

    refresh.defer();
    refresh.defer();
    expect(refresh.consume(true)).toBe(false);
    expect(refresh.consume(false)).toBe(true);
    expect(refresh.consume(false)).toBe(false);
  });
});

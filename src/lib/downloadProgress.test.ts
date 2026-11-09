import { describe, expect, it } from 'vitest';
import {
  countProgressPercent,
  fileProgressPercent,
  importProgressPercent,
  weightedProgressPercent,
} from './downloadProgress';

describe('download progress math', () => {
  it('increases a single file from 0 to 99 and only reaches 100 when done', () => {
    expect(fileProgressPercent(0, 100)).toBe(0);
    expect(fileProgressPercent(25, 100)).toBe(25);
    expect(fileProgressPercent(99, 100)).toBe(99);
    expect(fileProgressPercent(100, 100)).toBe(99);
    expect(fileProgressPercent(100, 100, true)).toBe(100);
  });

  it('returns indeterminate when the total is missing or invalid', () => {
    expect(fileProgressPercent(42, null)).toBeNull();
    expect(fileProgressPercent(42, 0)).toBeNull();
    expect(fileProgressPercent(42, -1)).toBeNull();
  });

  it('never returns negative or above-100 values', () => {
    expect(fileProgressPercent(-10, 100)).toBe(0);
    expect(fileProgressPercent(120, 100)).toBe(99);
    expect(fileProgressPercent(120, 100, true)).toBe(100);
  });

  it('weights multiple files by bytes instead of averaging their percentages', () => {
    // 10-byte file done + 50 of a 90-byte file = 60/100, not (100 + 55)/2.
    expect(weightedProgressPercent(10, 50, 100)).toBe(60);
    expect(weightedProgressPercent(10, 50, 0)).toBeNull();
    expect(weightedProgressPercent(100, 0, 100)).toBe(99);
    expect(weightedProgressPercent(100, 0, 100, true)).toBe(100);
  });

  it('uses a count fallback only when sizes are unavailable', () => {
    expect(countProgressPercent(0, 3)).toBe(0);
    expect(countProgressPercent(1, 3)).toBeCloseTo(33.33, 2);
    expect(countProgressPercent(3, 3)).toBe(99);
    expect(countProgressPercent(3, 3, true)).toBe(100);
    expect(countProgressPercent(0, 0)).toBeNull();
  });

  it('keeps modpack extraction/metadata stages indeterminate and reaches 100 only on done', () => {
    expect(importProgressPercent({ stage: 'extracting', current: 0, total: 1 })).toBeNull();
    expect(importProgressPercent({ stage: 'reading', current: 0, total: 1 })).toBeNull();
    expect(
      importProgressPercent({
        stage: 'downloading-mods',
        current: 10,
        total: 10,
      }),
    ).toBe(99);
    expect(
      importProgressPercent({
        stage: 'downloading-mods',
        current: 10,
        total: 10,
      }, 0),
    ).toBe(99);
    expect(importProgressPercent({ stage: 'done', current: 1, total: 1 })).toBe(100);
  });

  it('weights byte-aware modpack stages and includes the in-flight file', () => {
    expect(
      importProgressPercent(
        { stage: 'files', current: 1, total: 2, bytes_done: 40, bytes_total: 100 },
        20,
      ),
    ).toBe(60);
    expect(
      importProgressPercent(
        { stage: 'assets', current: 1, total: 2, bytes_done: 40, bytes_total: 100 },
        20,
      ),
    ).toBe(60);
    expect(
      importProgressPercent({
        stage: 'downloading-mods',
        current: 1,
        total: 2,
      }),
    ).toBe(50);
  });
});

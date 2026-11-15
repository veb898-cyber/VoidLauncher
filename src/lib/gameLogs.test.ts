import { describe, expect, it } from 'vitest';
import { Virtualizer } from '@tanstack/virtual-core';
import { filterLogLines, getLineLevel } from './gameLogs';

function buildLargeLog(): string[] {
  const lines: string[] = [];
  for (let i = 1; i <= 12_000; i += 1) {
    const level = i % 97 === 0 ? '[ERROR]' : i % 53 === 0 ? '[WARN]' : '[INFO]';
    lines.push(`${level} 2026-11-10 12:00:00 [Render thread/INFO] (example) line ${i}: ${'x'.repeat(18)}`);
  }
  lines.push(`[ERROR] stack trace: ${'at com.example.VeryLongFrame('.repeat(80)}`);
  lines.push('Сообщение на русском — Unicode survives filtering');
  return lines;
}

function createVirtualizer(count: number) {
  return new Virtualizer({
    count,
    getScrollElement: () => document.createElement('div'),
    estimateSize: () => 22,
    overscan: 20,
    initialRect: { width: 900, height: 600 },
    initialOffset: 0,
    observeElementRect: (_instance, cb) => {
      cb({ width: 900, height: 600 });
    },
    observeElementOffset: (_instance, cb) => {
      cb(0, false);
    },
    scrollToFn: () => {},
  });
}

function assertContiguous(items: Array<{ index: number }>, count: number) {
  expect(items.length).toBeGreaterThan(0);
  expect(new Set(items.map((item) => item.index)).size).toBe(items.length);
  for (let i = 1; i < items.length; i += 1) {
    expect(items[i].index).toBe(items[i - 1].index + 1);
  }
  expect(items[0].index).toBeGreaterThanOrEqual(0);
  expect(items[items.length - 1].index).toBeLessThan(count);
}

describe('large game logs', () => {
  const lines = buildLargeLog();

  it('builds a realistic log of more than 300 KB without losing lines', () => {
    expect(lines.length).toBe(12_002);
    expect(lines.join('\n').length).toBeGreaterThan(300 * 1024);
  });

  it('keeps search and level filtering over the complete in-memory log', () => {
    const all = filterLogLines(lines, '', { error: true, warn: true, info: true });
    expect(all).toHaveLength(lines.length);
    expect(all[0]).toBe(lines[0]);
    expect(all[all.length - 1]).toBe(lines[lines.length - 1]);

    const errors = filterLogLines(lines, '', { error: true, warn: false, info: false });
    expect(errors.length).toBeGreaterThan(100);
    expect(errors.every((line) => getLineLevel(line) === 'error')).toBe(true);

    const unicode = filterLogLines(lines, 'русском', { error: true, warn: true, info: true });
    expect(unicode).toEqual(['Сообщение на русском — Unicode survives filtering']);

    const warnings = filterLogLines(lines, 'line 530:', { error: true, warn: true, info: true });
    expect(warnings).toHaveLength(1);
    expect(warnings[0]).toContain('line 530:');
  });

  it('computes a contiguous visible range at the start, middle and end', () => {
    const virtualizer = createVirtualizer(lines.length);
    const total = virtualizer.getTotalSize();
    expect(total).toBeGreaterThan(lines.length * 20);

    for (const offset of [0, Math.floor(total / 2), total - 600]) {
      virtualizer.scrollOffset = offset;
      const items = virtualizer.getVirtualItems();
      assertContiguous(items, lines.length);
      if (offset === 0) expect(items[0].index).toBe(0);
      if (offset === total - 600) {
        expect(items[items.length - 1].index).toBe(lines.length - 1);
      }
    }
  });

  it('survives rapid alternating jumps without empty or duplicate ranges', () => {
    const virtualizer = createVirtualizer(lines.length);
    const total = virtualizer.getTotalSize();
    const jumps = [0, total - 600, 500, total / 3, total - 1200, 42, total - 600, 0];
    for (const offset of jumps) {
      virtualizer.scrollOffset = offset;
      assertContiguous(virtualizer.getVirtualItems(), lines.length);
    }
  });

  it('keeps indices contiguous after long rows are measured', () => {
    const virtualizer = createVirtualizer(lines.length);
    virtualizer.scrollOffset = 9000 * 22;
    virtualizer.getVirtualItems();
    virtualizer.resizeItem(9000, 240);
    const items = virtualizer.getVirtualItems();
    assertContiguous(items, lines.length);
    expect(virtualizer.getTotalSize()).toBeGreaterThan(lines.length * 22);
  });
});

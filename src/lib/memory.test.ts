import { describe, expect, it } from 'vitest';
import {
  MIN_MEMORY_MB,
  getMaxMemoryMb,
  getRecommendedMemoryMb,
  resolveInitialMemoryMb,
  snapToMemoryStep,
} from './memory';

describe('getRecommendedMemoryMb', () => {
  it('falls back to 4 GB for unknown or nonsensical RAM totals', () => {
    expect(getRecommendedMemoryMb(0)).toBe(4096);
    expect(getRecommendedMemoryMb(-1)).toBe(4096);
  });

  it('tiers by total RAM', () => {
    expect(getRecommendedMemoryMb(8 * 1024)).toBe(4096);
    expect(getRecommendedMemoryMb(16 * 1024)).toBe(6144);
    expect(getRecommendedMemoryMb(24 * 1024)).toBe(8192);
    expect(getRecommendedMemoryMb(64 * 1024)).toBe(8192);
  });
});

describe('getMaxMemoryMb', () => {
  it('reserves 2 GB for the OS and stays on the 512 MB grid', () => {
    expect(getMaxMemoryMb(8 * 1024)).toBe(6144);
    expect(getMaxMemoryMb(16 * 1024)).toBe(12800);
    expect(getMaxMemoryMb(64 * 1024)).toBe(52224);
  });

  it('never returns less than the hard floor', () => {
    expect(getMaxMemoryMb(1024)).toBe(MIN_MEMORY_MB);
    expect(getMaxMemoryMb(0)).toBe(MIN_MEMORY_MB);
  });
});

describe('snapToMemoryStep', () => {
  it('rounds to the nearest 512 MB step', () => {
    expect(snapToMemoryStep(2500)).toBe(2560);
    expect(snapToMemoryStep(4600)).toBe(4608);
  });

  it('clamps values below the minimum up to 2048 MB', () => {
    expect(snapToMemoryStep(100)).toBe(MIN_MEMORY_MB);
    expect(snapToMemoryStep(1500)).toBe(MIN_MEMORY_MB);
  });
});

describe('resolveInitialMemoryMb', () => {
  it('prefers the instance value when present', () => {
    expect(resolveInitialMemoryMb(6144, 16384, 8192)).toBe(6144);
  });

  it('clamps to [MIN, dynamic max]', () => {
    expect(resolveInitialMemoryMb(99999, 16384)).toBe(12800);
    expect(resolveInitialMemoryMb(512, 16384)).toBe(MIN_MEMORY_MB);
  });

  it('falls back to the global default, then to the recommendation', () => {
    expect(resolveInitialMemoryMb(null, 16384, 4096)).toBe(4096);
    expect(resolveInitialMemoryMb(null, 16384)).toBe(6144);
  });
});

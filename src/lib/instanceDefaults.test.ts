import { describe, expect, it } from 'vitest';
import { gcOverrideValue, memoryOverrideValue } from './instanceDefaults';

describe('instance default inheritance', () => {
  it('saves null while the instance uses global memory', () => {
    expect(memoryOverrideValue(true, 6144)).toBeNull();
    expect(memoryOverrideValue(false, 6144)).toBe(6144);
  });

  it('saves null only for inherit GC', () => {
    expect(gcOverrideValue('inherit')).toBeNull();
    expect(gcOverrideValue('g1gc')).toBe('g1gc');
    expect(gcOverrideValue('standard')).toBe('standard');
    expect(gcOverrideValue('zgc')).toBe('zgc');
  });

  it('inherits the two parameters independently', () => {
    expect(memoryOverrideValue(true, 8192)).toBeNull();
    expect(gcOverrideValue('zgc')).toBe('zgc');
    expect(memoryOverrideValue(false, 8192)).toBe(8192);
    expect(gcOverrideValue('inherit')).toBeNull();
  });
});

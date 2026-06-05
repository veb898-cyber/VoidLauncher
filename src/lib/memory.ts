/**
 * Memory allocation helpers.
 *
 * Recommendations:
 *   total RAM  ≤ 8 GB  → 4 GB default
 *   total RAM  = 16 GB → 6 GB default
 *   total RAM  ≥ 32 GB → 8 GB default (so ZGC preset is selectable)
 *
 * Returns MB. Rounded to the nearest 512 MB step.
 */

export const MIN_MEMORY_MB = 2048;        // 2 GB
export const MEMORY_STEP_MB = 512;
export const SYSTEM_RESERVE_MB = 2048;    // always leave 2 GB for the OS
export const MAX_MEMORY_PCT = 0.8;        // ... or 80% of total, whichever is lower

/** Tiered recommendation table (in MB). */
export function getRecommendedMemoryMb(totalRamMb: number): number {
  if (totalRamMb <= 0) return 4096;
  if (totalRamMb >= 32 * 1024) return 8192;   // ≥ 32 GB → 8 GB
  if (totalRamMb >= 16 * 1024) return 6144;   // = 16 GB  → 6 GB
  return 4096;                                // ≤ 8 GB   → 4 GB
}

/**
 * Dynamic max slider value:
 *   min(total - 2 GB, 80% of total), floored to 512 MB, hard floor 2 GB.
 */
export function getMaxMemoryMb(totalRamMb: number): number {
  const reserveBased = Math.max(totalRamMb - SYSTEM_RESERVE_MB, 0);
  const pctBased = Math.floor(totalRamMb * MAX_MEMORY_PCT);
  const dynamic = Math.max(0, Math.min(reserveBased, pctBased));
  const floored = Math.floor(dynamic / MEMORY_STEP_MB) * MEMORY_STEP_MB;
  return Math.max(MIN_MEMORY_MB, floored);
}

/** Round any MB value to the nearest 512 MB step. */
export function snapToMemoryStep(mb: number): number {
  return Math.max(MIN_MEMORY_MB, Math.round(mb / MEMORY_STEP_MB) * MEMORY_STEP_MB);
}

/**
 * Resolve the initial slider value:
 *   - if the instance already has a memory_mb → use it
 *   - else if a global default is known → use it
 *   - else fall back to the tiered recommendation
 * Always clamped to [MIN, max].
 */
export function resolveInitialMemoryMb(
  instanceMemoryMb: number | null | undefined,
  totalRamMb: number,
  globalDefaultMb?: number | null,
): number {
  const max = getMaxMemoryMb(totalRamMb);
  const clamp = (v: number) => Math.min(Math.max(v, MIN_MEMORY_MB), max);

  if (instanceMemoryMb && instanceMemoryMb > 0) return clamp(instanceMemoryMb);
  if (globalDefaultMb && globalDefaultMb > 0) return clamp(globalDefaultMb);
  return clamp(getRecommendedMemoryMb(totalRamMb));
}

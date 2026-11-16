/**
 * Memory allocation helpers.
 *
 * Recommendations:
 *   total RAM  < 16 GB → 4 GB default
 *   total RAM  = 16 GB → 6 GB default
 *   total RAM  ≥ 24 GB → 8 GB default (so ZGC preset is selectable)
 *
 * Returns MB. Rounded to the nearest 512 MB step.
 */

export const MIN_MEMORY_MB = 2048;        // 2 GB
export const MAX_MEMORY_MB = 32768;       // 32 GB, matches the backend hard cap
export const MEMORY_STEP_MB = 512;
export const SYSTEM_RESERVE_MB = 2048;    // always leave 2 GB for the OS
export const MAX_MEMORY_PCT = 0.8;        // ... or 80% of total, whichever is lower

/** Tiered recommendation table (in MB). */
export function getRecommendedMemoryMb(totalRamMb: number): number {
  if (totalRamMb <= 0) return 4096;
  if (totalRamMb >= 24 * 1024) return 8192;   // ≥ 24 GB → 8 GB
  if (totalRamMb >= 16 * 1024) return 6144;   // = 16 GB  → 6 GB
  return 4096;                                // < 16 GB  → 4 GB
}

/**
 * Safe global memory ceiling for a known RAM total.
 *
 * Reserves 2 GB or 20% for the OS, whichever is larger, caps at the absolute
 * backend limit and snaps to the 512 MB grid. `null` means the system cannot
 * safely provide even the minimum heap (or RAM is unknown), so callers must
 * keep the existing limit instead of inventing a new one.
 */
export function getSafeMaxMemoryMb(totalRamMb: number): number | null {
  if (!Number.isFinite(totalRamMb) || totalRamMb < 4096) return null;
  const usable = Math.max(totalRamMb - SYSTEM_RESERVE_MB, 0);
  const percentCap = Math.floor(totalRamMb * MAX_MEMORY_PCT);
  const safe = Math.min(usable, percentCap, MAX_MEMORY_MB);
  const floored = Math.floor(safe / MEMORY_STEP_MB) * MEMORY_STEP_MB;
  return floored >= MIN_MEMORY_MB ? floored : null;
}

/**
 * Legacy hard-floor helper used by the instance editor and initial-value
 * resolution. Prefer `getSafeMaxMemoryMb` when deciding what to offer in the
 * UI: it can return `null` for systems below the safe minimum.
 */
export function getMaxMemoryMb(totalRamMb: number): number {
  return getSafeMaxMemoryMb(totalRamMb) ?? MIN_MEMORY_MB;
}

/**
 * UI/launch ceiling for a specific instance editor session.
 *
 * - unknown RAM (`null` / 0) leaves the global cap (or hard cap) in place and
 *   never invents an 8 GB machine;
 * - a known system below 4 GB gets the hard floor and `ramInsufficient=true`;
 * - a known safe ceiling is additionally capped by the global limit.
 */
export function getEffectiveMemoryLimitMb(
  configMaxMb: number | null,
  totalRamMb: number | null,
): { maxMb: number; ramKnown: boolean; ramInsufficient: boolean } {
  const ramKnown = totalRamMb != null && totalRamMb > 0;
  const safe = ramKnown ? getSafeMaxMemoryMb(totalRamMb) : null;
  const ramInsufficient = ramKnown && safe === null;
  if (ramInsufficient) {
    return { maxMb: MIN_MEMORY_MB, ramKnown, ramInsufficient };
  }
  const configBound = configMaxMb ?? MAX_MEMORY_MB;
  const maxMb = Math.max(
    MIN_MEMORY_MB,
    safe !== null ? Math.min(safe, configBound) : configBound,
  );
  return { maxMb, ramKnown, ramInsufficient };
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

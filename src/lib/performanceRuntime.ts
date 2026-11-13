/**
 * Persistence and one-time migration for the optional interface-effect
 * reduction.
 *
 * The old build stored a performance profile plus a full effect map. Both are
 * gone now: the UI has a single switch, so only its boolean is kept. The
 * migration intentionally ignores wallpaper, Liquid Glass, the custom
 * scrollbar and the old 3D-skin flag: those are no longer optimization
 * choices, and the skin animation itself has been removed.
 */

export const REDUCE_EFFECTS_STORAGE_KEY = 'voidlauncher-reduce-interface-effects';
export const LEGACY_PROFILE_STORAGE_KEY = 'voidlauncher-performance-profile';
export const LEGACY_EFFECTS_STORAGE_KEY = 'voidlauncher-performance-effects';

/**
 * Effects that the new "reduce interface effects" switch used to control in
 * the old per-effect map. The old scrollbar, wallpaper and skin flags are
 * deliberately absent.
 */
const CONTROLLED_LEGACY_EFFECTS = [
  'heavyShadows',
  'hoverAnimations',
  'uiTransitions',
  'decorativeAnimations',
  'gooeyEffects',
] as const;

function readItem(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

function writeItem(key: string, value: string): void {
  try {
    localStorage.setItem(key, value);
  } catch {}
}

function removeItem(key: string): void {
  try {
    localStorage.removeItem(key);
  } catch {}
}

function parseEffects(value: string | null): Record<string, unknown> | null {
  if (!value) return null;
  try {
    const parsed: unknown = JSON.parse(value);
    return parsed && typeof parsed === 'object'
      ? (parsed as Record<string, unknown>)
      : null;
  } catch {
    return null;
  }
}

/**
 * Decide whether the old saved state expressed an intent to reduce effects.
 * The decision is intentionally conservative: only an explicitly disabled
 * controlled effect (or the old balanced/saver profile) turns the new switch
 * on. Unknown or unreadable data keeps the new safe default, "off".
 */
export function inferReduceInterfaceEffects(
  profile: string | null,
  effects: Record<string, unknown> | null,
): boolean {
  if (profile === 'balanced' || profile === 'saver') return true;
  if (profile === 'standard') return false;
  if (!effects) return false;
  return CONTROLLED_LEGACY_EFFECTS.some((id) => effects[id] === false);
}

function readStoredReduceEffects(): boolean | null {
  const value = readItem(REDUCE_EFFECTS_STORAGE_KEY);
  if (value === '1') return true;
  if (value === '0') return false;
  return null;
}

function removeLegacyKeys(): void {
  removeItem(LEGACY_PROFILE_STORAGE_KEY);
  removeItem(LEGACY_EFFECTS_STORAGE_KEY);
}

/**
 * Runs once on startup. A stored value for the new key always wins; legacy
 * keys are only read when the new key does not exist yet. Either way the
 * obsolete keys are removed so the migration cannot run again.
 */
export function migrateLegacyPerformanceSettings(): boolean {
  const stored = readStoredReduceEffects();
  if (stored !== null) {
    removeLegacyKeys();
    return stored;
  }

  const profile = readItem(LEGACY_PROFILE_STORAGE_KEY);
  const effects = parseEffects(readItem(LEGACY_EFFECTS_STORAGE_KEY));
  const reduce = inferReduceInterfaceEffects(profile, effects);
  writeItem(REDUCE_EFFECTS_STORAGE_KEY, reduce ? '1' : '0');
  removeLegacyKeys();
  return reduce;
}

export function writeReduceInterfaceEffects(enabled: boolean): void {
  writeItem(REDUCE_EFFECTS_STORAGE_KEY, enabled ? '1' : '0');
}

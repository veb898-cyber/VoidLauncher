/**
 * Display helpers for installed mods. The backend keeps three independent
 * concepts apart: the download source (Modrinth/CurseForge/Local), the mod
 * loaders supported by the JAR, and the mod version. These helpers only format
 * that data and never invent a version or a loader.
 */

/** Minecraft loaders this launcher supports. `Local` is a source, not a loader. */
export const MOD_LOADERS = ['Fabric', 'Forge', 'NeoForge'] as const;

export type ModLoader = (typeof MOD_LOADERS)[number];

const PLACEHOLDER_VERSION = /^(?:unknown|none|n\/a|null|-|\$\{.*\})$/i;

/** `true` only for a concrete version string, never for a build placeholder. */
export function isKnownModVersion(version: string | null | undefined): version is string {
  if (typeof version !== 'string') return false;
  const trimmed = version.trim();
  return trimmed !== '' && !PLACEHOLDER_VERSION.test(trimmed);
}

/**
 * Version shown in the list. An explicit `version_known === false` wins over a
 * stale string; old metadata without the flag still falls back to the value.
 */
export function modVersionLabel(
  version: string | null | undefined,
  versionKnown: boolean | undefined,
  unknownLabel: string,
): string {
  if (versionKnown === false) return unknownLabel;
  return isKnownModVersion(version) ? version.trim() : unknownLabel;
}

/** Keep only supported loaders, deduplicated, preserving the backend order. */
export function normalizeModLoaders(loaders: unknown): ModLoader[] {
  if (!Array.isArray(loaders)) return [];
  const seen = new Set<string>();
  const result: ModLoader[] = [];
  for (const value of loaders) {
    if (typeof value !== 'string') continue;
    const loader = value.trim();
    if (!(MOD_LOADERS as readonly string[]).includes(loader)) continue;
    if (seen.has(loader)) continue;
    seen.add(loader);
    result.push(loader as ModLoader);
  }
  return result;
}

/** `Fabric / NeoForge`; empty when the JAR has no supported descriptor. */
export function modLoadersLabel(loaders: unknown, separator = ' / '): string {
  return normalizeModLoaders(loaders).join(separator);
}

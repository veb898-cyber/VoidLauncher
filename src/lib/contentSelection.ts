/**
 * Merge a dependency-resolution plan with the user's current selection.
 *
 * The resolver may answer with a different version of a root project (for
 * example when a dependency edge was visited first). The user's explicit pin
 * is authoritative for that project: keep the selected item instead of letting
 * a late/async resolution response silently change it. Dependencies themselves
 * always come from the resolver.
 */

export interface SelectableContent {
  name: string;
  iconUrl?: string;
  downloadUrl: string;
  filename: string;
  source: string;
  projectId: string;
  versionId?: string;
  versionName?: string;
  isDependency?: boolean;
  parentMod?: string;
  sha1?: string;
  fileId?: string;
  size?: number;
}

function projectKey(item: Pick<SelectableContent, 'source' | 'projectId'>): string {
  return `${item.source || 'modrinth'}:${item.projectId}`;
}

export function pinSameProjectVersion<T extends SelectableContent>(
  items: readonly T[],
  item: T,
): T[] {
  const replacementKey = projectKey(item);
  const existing = items.findIndex((candidate) => projectKey(candidate) === replacementKey);
  if (existing < 0) return [...items, item];
  return items.reduce<T[]>((next, candidate, index) => {
    if (index === existing) next.push(item);
    else if (projectKey(candidate) !== replacementKey) next.push(candidate);
    return next;
  }, []);
}

export function mergeResolvedSelection<T extends SelectableContent>(
  current: readonly T[],
  resolved: readonly T[],
): T[] {
  const pinned = new Map<string, T>();
  for (const item of current) {
    if (!item.isDependency) pinned.set(projectKey(item), item);
  }
  return resolved.map((item) => {
    if (item.isDependency) return item;
    return pinned.get(projectKey(item)) ?? item;
  });
}

/** Remove every selected version of one project, regardless of the active catalog. */
export function removeProject<T extends SelectableContent>(
  items: readonly T[],
  provider: string | undefined,
  projectId: string,
): T[] {
  const key = `${provider || 'modrinth'}:${projectId}`;
  return items.filter((item) => projectKey(item) !== key);
}

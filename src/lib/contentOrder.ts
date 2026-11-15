export interface OrderableContent {
  filename: string;
  enabled: boolean;
}

/**
 * Move an active content item one step up (higher priority) or down (lower
 * priority) in display order. Disabled items are ignored and keep their
 * relative order after the active ones. Returns `null` when the move is not
 * possible (item disabled, missing, or already at the edge).
 */
export function moveActiveContent<T extends OrderableContent>(
  items: T[],
  filename: string,
  direction: 'up' | 'down',
): T[] | null {
  const active = items.filter((item) => item.enabled);
  const position = active.findIndex((item) => item.filename === filename);
  const target = direction === 'up' ? position - 1 : position + 1;
  if (position < 0 || target < 0 || target >= active.length) return null;

  const reordered = active.slice();
  [reordered[position], reordered[target]] = [reordered[target], reordered[position]];
  const disabled = items.filter((item) => !item.enabled);
  return [...reordered, ...disabled];
}

/**
 * Convert the UI display order (highest priority first) to the on-disk
 * `resourcePacks` order expected by the backend: low priority -> high
 * priority. Only active packs belong in options.txt.
 */
export function resourcePacksOptionsOrder<T extends OrderableContent>(items: T[]): string[] {
  return items
    .filter((item) => item.enabled)
    .map((item) => item.filename)
    .reverse();
}

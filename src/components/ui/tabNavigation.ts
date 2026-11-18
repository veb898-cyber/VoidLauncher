export interface TabNavigationItem<T extends string> {
  id: T;
  disabled?: boolean;
}

/**
 * Next enabled tab in the arrow-key direction, wrapping at the ends. Returns
 * null when there is no other enabled tab (a single-tab row or all disabled).
 */
export function adjacentEnabledTab<T extends string>(
  tabs: readonly TabNavigationItem<T>[],
  active: T,
  direction: 1 | -1,
): T | null {
  const start = tabs.findIndex((tab) => tab.id === active);
  if (start < 0) return null;

  for (let step = 1; step < tabs.length; step += 1) {
    const index = (start + direction * step + tabs.length) % tabs.length;
    const candidate = tabs[index];
    if (!candidate.disabled) return candidate.id;
  }
  return null;
}

/** First or last enabled tab for Home/End. */
export function edgeEnabledTab<T extends string>(
  tabs: readonly TabNavigationItem<T>[],
  edge: 'first' | 'last',
): T | null {
  const ordered = edge === 'first' ? tabs : [...tabs].reverse();
  return ordered.find((tab) => !tab.disabled)?.id ?? null;
}

/**
 * The tab that should own the row's single tab stop. Normally this is the
 * active tab; when it becomes disabled, the first enabled tab takes the stop
 * instead of leaving a disabled element as the only `tabIndex=0`.
 */
export function focusableTab<T extends string>(
  tabs: readonly TabNavigationItem<T>[],
  active: T,
): T | null {
  const current = tabs.find((tab) => tab.id === active);
  if (current && !current.disabled) return current.id;
  return edgeEnabledTab(tabs, 'first');
}

/**
 * Scroll offset that brings a tab fully into the row. A small gutter keeps the
 * tab from touching the clip edge. Returns the current offset when the tab is
 * already visible, so callers can skip a no-op scroll.
 */
export function revealedScrollOffset(
  itemStart: number,
  itemSize: number,
  viewStart: number,
  viewSize: number,
  gutter = 8,
): number {
  const start = Math.max(0, itemStart - gutter);
  const end = itemStart + itemSize + gutter;

  if (start < viewStart) return start;
  if (end > viewStart + viewSize) return Math.max(0, end - viewSize);
  return viewStart;
}

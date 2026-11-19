import { describe, expect, it } from 'vitest';
import {
  adjacentEnabledTab,
  edgeEnabledTab,
  focusableTab,
  revealedScrollOffset,
  type TabNavigationItem,
} from './tabNavigation';

const tabs: TabNavigationItem<'one' | 'two' | 'three' | 'four'>[] = [
  { id: 'one' },
  { id: 'two', disabled: true },
  { id: 'three' },
  { id: 'four' },
];

describe('tab keyboard navigation', () => {
  it('moves forward and backward, skipping disabled tabs and wrapping', () => {
    expect(adjacentEnabledTab(tabs, 'one', 1)).toBe('three');
    expect(adjacentEnabledTab(tabs, 'three', 1)).toBe('four');
    expect(adjacentEnabledTab(tabs, 'four', 1)).toBe('one');
    expect(adjacentEnabledTab(tabs, 'one', -1)).toBe('four');
    expect(adjacentEnabledTab(tabs, 'four', -1)).toBe('three');
    expect(adjacentEnabledTab(tabs, 'three', -1)).toBe('one');
  });

  it('returns null when there is no other enabled tab', () => {
    expect(adjacentEnabledTab([{ id: 'only' }], 'only', 1)).toBeNull();
    expect(
      adjacentEnabledTab(
        [{ id: 'one' }, { id: 'two', disabled: true }] as TabNavigationItem<'one' | 'two'>[],
        'one',
        1,
      ),
    ).toBeNull();
  });

  it('finds the first and last enabled tab for Home/End', () => {
    expect(edgeEnabledTab(tabs, 'first')).toBe('one');
    expect(edgeEnabledTab(tabs, 'last')).toBe('four');
    expect(
      edgeEnabledTab(
        [{ id: 'one', disabled: true }, { id: 'two' }, { id: 'three', disabled: true }] as TabNavigationItem<
          'one' | 'two' | 'three'
        >[],
        'first',
      ),
    ).toBe('two');
    expect(edgeEnabledTab([{ id: 'one', disabled: true }], 'last')).toBeNull();
  });

  it('moves the single tab stop away from a disabled active tab', () => {
    expect(focusableTab(tabs, 'one')).toBe('one');
    expect(focusableTab(tabs, 'two')).toBe('one');
    expect(
      focusableTab(
        [{ id: 'one', disabled: true }, { id: 'two' }, { id: 'three' }] as TabNavigationItem<
          'one' | 'two' | 'three'
        >[],
        'one',
      ),
    ).toBe('two');
    expect(focusableTab([{ id: 'only', disabled: true }], 'only')).toBeNull();
  });
});

describe('tab reveal offsets', () => {
  it('keeps the current offset when the tab is already visible', () => {
    expect(revealedScrollOffset(120, 80, 100, 300)).toBe(100);
  });

  it('scrolls left with a gutter when the tab starts before the view', () => {
    expect(revealedScrollOffset(40, 80, 100, 300)).toBe(32);
    expect(revealedScrollOffset(0, 80, 100, 300)).toBe(0);
  });

  it('scrolls right just enough to reveal the tab and its gutter', () => {
    expect(revealedScrollOffset(380, 100, 100, 300)).toBe(188);
  });

  it('clamps a tab wider than the view to a valid non-negative offset', () => {
    expect(revealedScrollOffset(20, 500, 0, 300)).toBe(228);
  });
});

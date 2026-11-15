import { describe, expect, it } from 'vitest';
import {
  isServerEditingLocked,
  responseIsCurrent,
  serversViewState,
  shouldReloadAfterRunningChange,
  shouldReloadOnFocus,
} from './serversListState';

describe('servers list lifecycle', () => {
  it('reloads on a real focus return, not on every render', () => {
    expect(shouldReloadOnFocus(false, true)).toBe(true);
    expect(shouldReloadOnFocus(true, true)).toBe(false);
    expect(shouldReloadOnFocus(true, false)).toBe(false);
  });

  it('reloads exactly when the tracked game for this instance exits', () => {
    expect(shouldReloadAfterRunningChange(true, false)).toBe(true);
    expect(shouldReloadAfterRunningChange(false, true)).toBe(false);
    expect(shouldReloadAfterRunningChange(false, false)).toBe(false);
  });

  it('locks editing only for the running instance', () => {
    expect(isServerEditingLocked(['Pack'], 'Pack')).toBe(true);
    expect(isServerEditingLocked(['Pack'], 'Other')).toBe(false);
    expect(isServerEditingLocked([], 'Pack')).toBe(false);
  });

  it('rejects a late response from an old request or another instance', () => {
    expect(
      responseIsCurrent({
        responseSeq: 2,
        currentSeq: 2,
        responseInstance: 'Pack',
        currentInstance: 'Pack',
      }),
    ).toBe(true);
    expect(
      responseIsCurrent({
        responseSeq: 1,
        currentSeq: 2,
        responseInstance: 'Pack',
        currentInstance: 'Pack',
      }),
    ).toBe(false);
    expect(
      responseIsCurrent({
        responseSeq: 2,
        currentSeq: 2,
        responseInstance: 'Pack',
        currentInstance: 'Other',
      }),
    ).toBe(false);
  });
});

describe('servers view state', () => {
  it('does not render a read error as an empty list', () => {
    expect(
      serversViewState({ loading: false, hasLoaded: false, error: 'read failed', count: 0 }),
    ).toBe('error');
  });

  it('shows loading only before the first successful read', () => {
    expect(
      serversViewState({ loading: true, hasLoaded: false, error: null, count: 0 }),
    ).toBe('loading');
    expect(
      serversViewState({ loading: true, hasLoaded: true, error: null, count: 2 }),
    ).toBe('list');
  });

  it('shows empty only after a successful empty read', () => {
    expect(
      serversViewState({ loading: false, hasLoaded: true, error: null, count: 0 }),
    ).toBe('empty');
    expect(
      serversViewState({ loading: false, hasLoaded: true, error: null, count: 1 }),
    ).toBe('list');
  });

  it('keeps an existing list on a refresh error', () => {
    expect(
      serversViewState({ loading: false, hasLoaded: true, error: 'refresh failed', count: 2 }),
    ).toBe('list');
  });
});

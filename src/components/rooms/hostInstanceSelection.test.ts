import { describe, expect, it } from 'vitest';
import {
  resolveHostInstanceSelection,
  type HostInstanceSelectionInput,
} from './hostInstanceSelection';

const base: HostInstanceSelectionInput = {
  savedInstance: null,
  instanceNames: ['Alpha', 'Beta'],
  instancesLoaded: true,
  currentSelection: '',
  pendingSelection: null,
};

describe('host instance selection', () => {
  it('restores the saved host instance instead of taking the first one', () => {
    const result = resolveHostInstanceSelection({
      ...base,
      savedInstance: 'Beta',
      currentSelection: '',
    });
    expect(result).toEqual({
      selection: 'Beta',
      pendingSelection: null,
      missingSavedInstance: false,
    });
  });

  it('waits for the instance list when the room status arrives first', () => {
    const beforeList = resolveHostInstanceSelection({
      ...base,
      savedInstance: 'Beta',
      instanceNames: [],
      instancesLoaded: false,
    });
    expect(beforeList.selection).toBe('');

    const afterList = resolveHostInstanceSelection({
      ...base,
      savedInstance: 'Beta',
      instanceNames: ['Alpha', 'Beta'],
      instancesLoaded: true,
    });
    expect(afterList.selection).toBe('Beta');
  });

  it('uses the saved instance when only the status arrives later', () => {
    const beforeStatus = resolveHostInstanceSelection({
      ...base,
      savedInstance: null,
      instanceNames: ['Alpha', 'Beta'],
      instancesLoaded: true,
      currentSelection: 'Alpha',
    });
    expect(beforeStatus.selection).toBe('Alpha');

    const afterStatus = resolveHostInstanceSelection({
      ...base,
      savedInstance: 'Beta',
      instanceNames: ['Alpha', 'Beta'],
      instancesLoaded: true,
      currentSelection: 'Alpha',
    });
    expect(afterStatus.selection).toBe('Beta');
  });

  it('keeps an explicit selection while the status still reports the old binding', () => {
    const waiting = resolveHostInstanceSelection({
      ...base,
      savedInstance: 'Alpha',
      currentSelection: 'Beta',
      pendingSelection: 'Beta',
    });
    expect(waiting).toEqual({
      selection: 'Beta',
      pendingSelection: 'Beta',
      missingSavedInstance: false,
    });

    const confirmed = resolveHostInstanceSelection({
      ...base,
      savedInstance: 'Beta',
      currentSelection: 'Beta',
      pendingSelection: 'Beta',
    });
    expect(confirmed).toEqual({
      selection: 'Beta',
      pendingSelection: null,
      missingSavedInstance: false,
    });
  });

  it('does not select the first instance over a missing saved binding', () => {
    const result = resolveHostInstanceSelection({
      ...base,
      savedInstance: 'DeletedPack',
      instanceNames: ['Alpha', 'Beta'],
      instancesLoaded: true,
      currentSelection: '',
    });
    expect(result.selection).toBe('');
    expect(result.missingSavedInstance).toBe(true);
  });

  it('does not report a missing saved instance before the list is loaded', () => {
    const result = resolveHostInstanceSelection({
      ...base,
      savedInstance: 'DeletedPack',
      instanceNames: [],
      instancesLoaded: false,
    });
    expect(result.missingSavedInstance).toBe(false);
    expect(result.selection).toBe('');
  });

  it('uses a display-only default when no instance is registered', () => {
    const result = resolveHostInstanceSelection({
      ...base,
      savedInstance: null,
      instanceNames: ['Alpha', 'Beta'],
      instancesLoaded: true,
      currentSelection: '',
    });
    expect(result.selection).toBe('Alpha');
    expect(result.missingSavedInstance).toBe(false);
  });

  it('keeps a valid current selection when there is no binding', () => {
    const result = resolveHostInstanceSelection({
      ...base,
      savedInstance: null,
      instanceNames: ['Alpha', 'Beta'],
      instancesLoaded: true,
      currentSelection: 'Beta',
    });
    expect(result.selection).toBe('Beta');
  });

  it('handles an empty instance list without inventing a selection', () => {
    const result = resolveHostInstanceSelection({
      ...base,
      savedInstance: null,
      instanceNames: [],
      instancesLoaded: true,
    });
    expect(result).toEqual({
      selection: '',
      pendingSelection: null,
      missingSavedInstance: false,
    });
  });
});

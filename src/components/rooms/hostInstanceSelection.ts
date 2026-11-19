/**
 * Selection rules for the host room's Minecraft instance.
 *
 * The backend is the source of truth: `hostInstance` is the instance that
 * currently owns the room and its monitored LAN port. The UI must never
 * replace that binding with the first item of the list just because the Rooms
 * page was opened. A different instance is registered only after the user
 * explicitly picks it (or explicitly launches/detects with it), and such a
 * pending change must survive the status poll that still carries the old
 * binding for a moment.
 */

export interface HostInstanceSelectionInput {
  /** Persisted/registered instance from `cmd_room_status`. */
  savedInstance: string | null;
  /** Names currently present in the instance list. */
  instanceNames: readonly string[];
  /** False while the list is still being fetched on the first load. */
  instancesLoaded: boolean;
  /** Current local selection in the host view. */
  currentSelection: string;
  /** Explicitly picked instance awaiting backend confirmation. */
  pendingSelection: string | null;
}

export interface HostInstanceSelection {
  selection: string;
  /** Still waiting for `savedInstance` to catch up with the explicit choice. */
  pendingSelection: string | null;
  /** The saved instance no longer exists in the list. */
  missingSavedInstance: boolean;
}

/**
 * Decide what the select should show without registering anything.
 *
 * - a saved instance wins whenever it still exists;
 * - an explicit choice wins until the backend confirms it;
 * - a missing saved instance is reported instead of silently selecting the
 *   first instance over the active (but stale) binding;
 * - with no saved binding the first instance is only a display default, not a
 *   backend binding.
 */
export function resolveHostInstanceSelection(
  input: HostInstanceSelectionInput,
): HostInstanceSelection {
  const saved = input.savedInstance?.trim() ? input.savedInstance.trim() : null;
  const names = input.instanceNames;

  if (input.pendingSelection) {
    if (saved === input.pendingSelection) {
      return {
        selection: input.pendingSelection,
        pendingSelection: null,
        missingSavedInstance: false,
      };
    }
    // Keep the explicit choice visible while the status poll still reports
    // the previous binding. Never snap back to the old instance.
    return {
      selection: input.currentSelection,
      pendingSelection: input.pendingSelection,
      missingSavedInstance: false,
    };
  }

  if (saved) {
    if (names.includes(saved)) {
      return {
        selection: saved,
        pendingSelection: null,
        missingSavedInstance: false,
      };
    }
    if (!input.instancesLoaded) {
      // The list may simply not be here yet; do not treat the saved instance
      // as deleted and do not bind another one.
      return {
        selection: input.currentSelection,
        pendingSelection: null,
        missingSavedInstance: false,
      };
    }
    // The saved instance is gone. Leave the select empty and show a notice;
    // the stale backend binding is cleared only by an explicit user action.
    return {
      selection: '',
      pendingSelection: null,
      missingSavedInstance: true,
    };
  }

  if (input.currentSelection && names.includes(input.currentSelection)) {
    return {
      selection: input.currentSelection,
      pendingSelection: null,
      missingSavedInstance: false,
    };
  }

  return {
    selection: names[0] ?? '',
    pendingSelection: null,
    missingSavedInstance: false,
  };
}

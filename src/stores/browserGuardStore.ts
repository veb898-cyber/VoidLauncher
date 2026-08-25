import { create } from 'zustand';

interface BrowserGuardState {
  /// Number of items currently selected in the content browser (mods/resourcepacks/shaders).
  /// Used to warn the user before leaving (tab switch, page switch, instance switch)
  /// and losing the selection.
  pending: number;
  /// True while a page has unsaved changes (e.g. global Settings). Leaving the
  /// page would discard them, so navigation asks for confirmation first.
  unsaved: boolean;
  /// Pending leave action stashed by `askLeave` while the confirmation dialog is open.
  request: (() => void) | null;
  setPending: (n: number) => void;
  setUnsaved: (v: boolean) => void;
  clear: () => void;
  /// Runs `action` immediately if nothing needs confirmation, otherwise opens
  /// the global confirmation dialog and stashes `action` until confirmed.
  askLeave: (action: () => void) => void;
  /// User confirmed leaving: discard selection/changes and run the stashed action.
  resolveLeave: () => void;
  cancelLeave: () => void;
}

export const useBrowserGuardStore = create<BrowserGuardState>((set, get) => ({
  pending: 0,
  unsaved: false,
  request: null,
  setPending: (n) => set({ pending: n }),
  setUnsaved: (v) => set({ unsaved: v }),
  clear: () => set({ pending: 0 }),
  askLeave: (action) => {
    if (get().pending > 0 || get().unsaved) {
      set({ request: action });
    } else {
      action();
    }
  },
  resolveLeave: () => {
    const { request } = get();
    set({ pending: 0, unsaved: false, request: null });
    request?.();
  },
  cancelLeave: () => set({ request: null }),
}));